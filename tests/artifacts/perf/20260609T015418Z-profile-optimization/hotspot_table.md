# Hotspot Table

| Rank | Location | Metric | Value | Category | Evidence |
|------|----------|--------|------:|----------|----------|
| 1 | `ftui_widgets::paragraph::Paragraph::new` -> `Text::into_owned` | allocation calls | 439,134 calls | alloc/CPU | `heaptrack_profile_sweep_cycles50_print.txt:5`; stack reaches `crates/ftui-widgets/src/paragraph.rs:97` |
| 2 | `ftui-demo-showcase` chrome tab/status text construction | allocation calls | 153,000 tab-bar paragraph calls plus related format/text allocations | alloc/CPU | `heaptrack_profile_sweep_cycles50_print.txt:89`, `:468`, `:796`; stack reaches `src/chrome.rs:390` and `src/chrome.rs:959` |
| 3 | Markdown math conversion (`unicodeit::replace` via `latex_to_unicode`) | allocation calls | 159,611 calls | alloc/CPU | `heaptrack_profile_sweep_cycles50_print.txt:1486`; stack reaches `unicodeit::replace`, `latex_to_unicode`, `cached_latex_to_unicode` |
| 4 | `performance_hud` screen | allocations/frame p95 | 6,191 | alloc/tail | `top_screens_by_alloc_p95.tsv:1`; frame p95 676us |
| 5 | `quake_easter_egg` screen | frame p99 | 3,764us | CPU | `top_screens_by_p99.tsv:1`; p95 2,696us |
| 6 | `markdown_rich_text` screen | frame p99 + allocations | 1,804us p99, 1,298 allocs/frame p95 | CPU/alloc | `top_screens_by_p99.tsv:2`; `top_screens_by_alloc_p95.tsv:3` |
| 7 | `dashboard` screen | frame p99 + allocations | 1,569us p99, 1,350 allocs/frame p95 | CPU/alloc | `top_screens_by_p99.tsv:4`; `top_screens_by_alloc_p95.tsv:2` |
| 8 | `BufferDiff` sparse/full diff | isolated time | dirty sparse 200x60 around 8.7us vs full around 21.5us | CPU | `criterion_ftui_render_diff_serial.txt`; presenter p99 only 104us in baseline |
| 9 | Syscalls / terminal I/O | syscall time | 4.732ms over 50 cycles | I/O | `strace_profile_sweep_cycles50.txt`; in-memory presenter sink |
| 10 | FrameArena mode | p95/elapsed | on: 2.925s, p95 729us; off: 2.899s, p95 710us | rejected lever | `arena_compare.jsonl` |

## Opportunity Matrix

| Rank | Lever | Impact | Confidence | Effort | Score | Recommendation |
|------|-------|:------:|:----------:|:------:|:-----:|----------------|
| 1 | Avoid repeated `Paragraph::new`/`Text::into_owned` for chrome/static labels | 5 | 5 | 2 | 12.5 | Do first |
| 2 | Strengthen markdown math cache so repeated snippets do not hit `unicodeit::replace` | 5 | 4 | 2 | 10.0 | Do early |
| 3 | Reduce `performance_hud` per-frame allocations | 4 | 4 | 2 | 8.0 | Do early |
| 4 | Cache or precompute dashboard/text panel content that is stable across frames | 4 | 4 | 2 | 8.0 | Do early |
| 5 | Quake/visual effects frame-time cleanup | 4 | 3 | 3 | 4.0 | Do after allocation passes |
| 6 | Additional dirty-diff tuning | 2 | 3 | 3 | 2.0 | Lower priority; not whole-system dominant |
| 7 | Presenter style emission tuning | 2 | 3 | 3 | 2.0 | Lower priority; presenter p99 is small |
| 8 | FrameArena expansion | 1 | 5 | 3 | 1.7 | Do not pursue now |

