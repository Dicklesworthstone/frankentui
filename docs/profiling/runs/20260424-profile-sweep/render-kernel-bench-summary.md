# Render Kernel Bench Summary

Run ID: `20260424T161752Z-profile-sweep`

This file preserves the useful Criterion result rows from the raw RCH bench logs
without retaining transfer/build console output.

## Presenter Pipeline

Command: `cargo bench -p ftui-render --bench presenter_bench -- pipeline/truecolor/diff_and_present --noplot`

| Case | Median | Range | Throughput median |
|---|---:|---:|---:|
| `pipeline/truecolor/diff_and_present/full/80x24@5%` | 18.251 us | 17.692-18.948 us | 105.20 Melem/s |
| `pipeline/truecolor/diff_and_present/full/80x24@50%` | 39.951 us | 39.850-40.115 us | 48.059 Melem/s |
| `pipeline/truecolor/diff_and_present/full/200x60@5%` | 76.683 us | 76.249-77.165 us | 156.49 Melem/s |
| `pipeline/truecolor/diff_and_present/full/200x60@50%` | 87.115 us | 86.717-87.575 us | 137.75 Melem/s |

## Diff Span Sparse Stats

Command: `cargo bench -p ftui-render --bench diff_bench -- diff/span_sparse_stats --noplot`

| Case | Method | Median | Range | Throughput median |
|---|---|---:|---:|---:|
| `200x60@sparse_5pct` | `compute` | 19.530 us | 19.434-19.647 us | 614.43 Melem/s |
| `200x60@sparse_5pct` | `compute_dirty` | 10.427 us | 10.368-10.493 us | 1.1509 Gelem/s |
| `200x60@single_row` | `compute` | 21.082 us | 20.561-21.681 us | 569.21 Melem/s |
| `200x60@single_row` | `compute_dirty` | 11.511 us | 11.449-11.579 us | 1.0425 Gelem/s |
| `240x80@sparse_5pct` | `compute` | 32.223 us | 32.076-32.400 us | 595.85 Melem/s |
| `240x80@sparse_5pct` | `compute_dirty` | 8.309 us | 8.236-8.408 us | 2.3108 Gelem/s |
| `240x80@single_row` | `compute` | 30.993 us | 30.845-31.158 us | 619.49 Melem/s |
| `240x80@single_row` | `compute_dirty` | 18.600 us | 18.550-18.655 us | 1.0323 Gelem/s |

## Diff Tile Sparse Stats

Command: `cargo bench -p ftui-render --bench diff_bench -- diff/tile_sparse_stats --noplot`

| Case | Method | Median | Range | Throughput median |
|---|---|---:|---:|---:|
| `320x90@1%` | `compute` | 50.085 us | 48.861-51.437 us | 575.02 Melem/s |
| `320x90@1%` | `compute_dirty` | 20.403 us | 20.317-20.501 us | 1.4116 Gelem/s |
| `320x90@2%` | `compute` | 47.410 us | 47.280-47.565 us | 607.46 Melem/s |
| `320x90@2%` | `compute_dirty` | 22.461 us | 22.332-22.604 us | 1.2822 Gelem/s |
| `400x100@1%` | `compute` | 66.265 us | 65.928-66.629 us | 603.64 Melem/s |
| `400x100@1%` | `compute_dirty` | 14.466 us | 14.377-14.573 us | 2.7651 Gelem/s |
| `400x100@2%` | `compute` | 66.270 us | 65.849-66.759 us | 603.59 Melem/s |
| `400x100@2%` | `compute_dirty` | 14.644 us | 14.547-14.752 us | 2.7315 Gelem/s |
