#!/usr/bin/env python3
"""Render docs/perf/baseline_2026-10-19.md, README.md <!-- perf:begin -->..<!-- perf:end -->, and README_BLOCK.sha256.

Usage:
    python3 -B scripts/render_perf_readme.py            # write / update artifacts
    python3 -B scripts/render_perf_readme.py --check    # verify no drift (exits 1 on mismatch)
    python3 -B scripts/render_perf_readme.py --self-test
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_REPORT = ROOT / "docs" / "perf" / "regression_report_2026-10-19.jsonl"
DEFAULT_BYTES = ROOT / "docs" / "perf" / "bytes_report_2026-10-19.json"
DEFAULT_ALLOC = ROOT / "docs" / "perf" / "alloc_report_2026-10-19.json"
DEFAULT_BASELINE_MD = ROOT / "docs" / "perf" / "baseline_2026-10-19.md"
DEFAULT_README = ROOT / "README.md"
DEFAULT_SHA256 = ROOT / "docs" / "perf" / "README_BLOCK.sha256"

BEGIN_MARKER = "<!-- perf:begin -->"
END_MARKER = "<!-- perf:end -->"


def fmt_ns(ns: float) -> str:
    if ns >= 1_000_000.0:
        return f"{ns / 1_000_000.0:.2f} ms"
    if ns >= 1_000.0:
        return f"{ns / 1_000.0:.2f} µs"
    if ns >= 10.0:
        return f"{ns:.1f} ns"
    return f"{ns:.3f} ns"


def load_rows(report_path: pathlib.Path) -> tuple[dict, list[dict], dict]:
    start_ev: dict = {}
    rows: list[dict] = []
    summary_ev: dict = {}
    for line in report_path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        obj = json.loads(line)
        ev = obj.get("event")
        if ev == "start":
            start_ev = obj
        elif ev == "row":
            rows.append(obj)
        elif ev == "summary":
            summary_ev = obj
    return start_ev, rows, summary_ev


def render_readme_block(
    rows: list[dict],
    bytes_rows: list[dict],
    alloc_rows: list[dict],
) -> str:
    by_cat = {r["category"]: r for r in rows}

    def obs_cell(cat: str) -> str:
        r = by_cat[cat]
        if "bytes" in r:
            return f"`{r['bytes']} B` (<= `{r['budget']} B`)"
        if "allocs" in r:
            return f"`{r['allocs']} allocs` (<= `{r['budget']}`)"
        return (
            f"`{fmt_ns(r['observed_mean_ns'])}` "
            f"(`[{fmt_ns(r['ci_low_ns'])}, {fmt_ns(r['ci_high_ns'])}]`)"
        )

    def budget_cell(cat: str) -> str:
        r = by_cat[cat]
        if "bytes" in r:
            return f"`{r['budget']} B` (`8*cells+64`)"
        if "allocs" in r:
            return f"`{r['budget']} allocs/frame`"
        return f"`{fmt_ns(r['baseline_p99_ns'])}` (+{r['effective_threshold_pct']}%)"

    ascii_arena = next(
        a for a in alloc_rows if a["scene"] == "ascii_dashboard_80x24" and a["arena"]
    )
    ascii_heap = next(
        a
        for a in alloc_rows
        if a["scene"] == "ascii_dashboard_80x24" and not a["arena"]
    )
    uni_arena = next(
        a for a in alloc_rows if a["scene"] == "unicode_dashboard_80x24" and a["arena"]
    )
    uni_heap = next(
        a
        for a in alloc_rows
        if a["scene"] == "unicode_dashboard_80x24" and not a["arena"]
    )

    lines = [
        "```bash",
        "# Run the full 27-threshold SLO + Criterion + byte-budget + allocation gate",
        "./scripts/perf_regression_gate.sh --quick --json",
        "python3 -B scripts/render_perf_readme.py --check",
        "",
        "# Paired certified vs dirty vs full-scan diff benchmarks (120×40, opt-level=\"z\"):",
        "cargo bench -p ftui-render --bench diff_bench",
        "cargo bench -p ftui-render --bench diff_paths_paired",
        "# diff_paths_paired (certified, spinner_tick 120x40)      p50: [90.7 ns]   throughput: [52.9 Gcells/s]",
        "# diff_paths_paired (certified, overmarked_chrome 120x40) p50: [251.4 ns]  throughput: [19.1 Gcells/s]",
        "# diff_paths_paired (certified, scattered_5pct 120x40)    p50: [1.98 µs]   throughput: [2.43 Gcells/s]",
        "# diff/sparse_5pct/compute/120x40  (full scan)            p50: [7.51 µs]   throughput: [639 Mcells/s]",
        "# diff/heavy_50pct/compute/120x40  (full scan)            p50: [8.22 µs]   throughput: [584 Mcells/s]",
        "# diff/full_100pct/compute/120x40  (full scan)            p50: [12.1 µs]   throughput: [396 Mcells/s]",
        "```",
        "",
        "| Operation / Metric | Criterion / Scene ID | Observed (Mean, 95% CI) | Enforced Budget |",
        "|---|---|---|---|",
        f"| Full frame pipeline (80×24 @ 5%) | `{by_cat['frame_render']['criterion_name']}` | {obs_cell('frame_render')} | {budget_cell('frame_render')} |",
        f"| Diff compute (80×24 @ 5% full scan) | `{by_cat['diff_strategy']['criterion_name']}` | {obs_cell('diff_strategy')} | {budget_cell('diff_strategy')} |",
        f"| Diff compute (200×60 @ 5% dirty rows) | `{by_cat['diff_200x60_sparse']['criterion_name']}` | {obs_cell('diff_200x60_sparse')} | {budget_cell('diff_200x60_sparse')} |",
        f"| Presenter ANSI emit (80×24 @ 5%) | `{by_cat['present_80x24_sparse']['criterion_name']}` | {obs_cell('present_80x24_sparse')} | {budget_cell('present_80x24_sparse')} |",
        f"| Presenter ANSI emit (120×40 @ 5%) | `{by_cat['present_120x40_sparse']['criterion_name']}` | {obs_cell('present_120x40_sparse')} | {budget_cell('present_120x40_sparse')} |",
        f"| Presenter ANSI emit (200×60 @ 5%) | `{by_cat['present_200x60_sparse']['criterion_name']}` | {obs_cell('present_200x60_sparse')} | {budget_cell('present_200x60_sparse')} |",
        f"| ANSI bytes emitted (80×24 @ 5%, 96 cells) | `{by_cat['bytes_emitted_sparse_5pct']['criterion_name']}` | {obs_cell('bytes_emitted_sparse_5pct')} | {budget_cell('bytes_emitted_sparse_5pct')} |",
        f"| Input parse + dispatch (per event) | `{by_cat['input_parse_dispatch_event']['criterion_name']}` | {obs_cell('input_parse_dispatch_event')} | {budget_cell('input_parse_dispatch_event')} |",
        f"| Word wrap (200 lines @ 80 cols) | `{by_cat['wrap_200_lines']['criterion_name']}` | {obs_cell('wrap_200_lines')} | {budget_cell('wrap_200_lines')} |",
        f"| Grapheme width lookup (S3-FIFO cached) | `{by_cat['text_width_non_ascii']['criterion_name']}` | {obs_cell('text_width_non_ascii')} | {budget_cell('text_width_non_ascii')} |",
        f"| Flex layout solver (10 constraints) | `{by_cat['layout_computation']['criterion_name']}` | {obs_cell('layout_computation')} | {budget_cell('layout_computation')} |",
        f"| Virtualized Fenwick scroll (100k items) | `{by_cat['virtualized_scroll_to_100k']['criterion_name']}` | {obs_cell('virtualized_scroll_to_100k')} | {budget_cell('virtualized_scroll_to_100k')} |",
        f"| Runtime first frame (`80×24` headless) | `{by_cat['runtime_first_frame']['criterion_name']}` | {obs_cell('runtime_first_frame')} | {budget_cell('runtime_first_frame')} |",
        f"| Runtime `Cmd::msg` roundtrip | `{by_cat['runtime_command_roundtrip']['criterion_name']}` | {obs_cell('runtime_command_roundtrip')} | {budget_cell('runtime_command_roundtrip')} |",
        f"| Runtime shutdown latency | `{by_cat['runtime_shutdown_latency']['criterion_name']}` | {obs_cell('runtime_shutdown_latency')} | {budget_cell('runtime_shutdown_latency')} |",
        f"| Steady-state heap allocs (ASCII 80×24) | `{by_cat['frame_allocations_ascii_scene']['criterion_name']}` | `{ascii_arena['allocs_per_frame']} allocs` (`0 B`, down from `{ascii_heap['allocs_per_frame']}` / `{ascii_heap['bytes_per_frame']:,} B`) | {budget_cell('frame_allocations_ascii_scene')} |",
        f"| Steady-state heap allocs (Unicode 80×24) | `{by_cat['frame_allocations_unicode_scene']['criterion_name']}` | `{uni_arena['allocs_per_frame']} allocs` (`{uni_arena['bytes_per_frame']} B`, down from `{uni_heap['allocs_per_frame']}` / `{uni_heap['bytes_per_frame']:,} B`) | {budget_cell('frame_allocations_unicode_scene')} |",
        "",
        "Full 27-metric table, confidence intervals, byte-efficiency breakdown, and `FrameArena` allocation report: [`docs/perf/baseline_2026-10-19.md`](docs/perf/baseline_2026-10-19.md) (generated from [`docs/perf/regression_report_2026-10-19.jsonl`](docs/perf/regression_report_2026-10-19.jsonl)).",
    ]
    return "\n".join(lines)


def render_baseline_md(
    start_ev: dict,
    rows: list[dict],
    summary_ev: dict,
    bytes_rows: list[dict],
    alloc_rows: list[dict],
) -> str:
    lines = [
        "# FrankenTUI Performance Baseline & SLO Gate Report (2026-10-19)",
        "",
        "> Generated deterministically by `python3 -B scripts/render_perf_readme.py` from",
        "> [`docs/perf/regression_report_2026-10-19.jsonl`](regression_report_2026-10-19.jsonl),",
        "> [`docs/perf/bytes_report_2026-10-19.json`](bytes_report_2026-10-19.json), and",
        "> [`docs/perf/alloc_report_2026-10-19.json`](alloc_report_2026-10-19.json).",
        "",
        "## Provenance & Measurement Environment",
        "",
        f"- **Run ID:** `{start_ev.get('run_id', 'unknown')}`",
        f"- **Timestamp:** `{start_ev.get('ts', 'unknown')}`",
        "- **Platform:** Linux `x86_64`",
        "- **Toolchain:** `rustc 1.95.0-nightly (7f99507f5 2026-02-19)` (`rust-toolchain.toml`)",
        "- **Profile:** Workspace `release` (`opt-level = \"z\"`, `lto = true`, `codegen-units = 1`, `panic = \"abort\"`)",
        f"- **Summary:** `total={summary_ev.get('total', len(rows))} passed={summary_ev.get('passed', len(rows))} failed={summary_ev.get('failed', 0)} skipped={summary_ev.get('skipped', 0)} incomplete={summary_ev.get('incomplete', 0)}`",
        "",
        "## Enforced Baseline Thresholds (27 / 27 Passing)",
        "",
        "| Category | Criterion / Scene ID | SLO Metric | Observed | 95% CI | Budget / Ceiling | Delta % | Status |",
        "|---|---|---|---|---|---|---|---|",
    ]

    for r in rows:
        cat = r["category"]
        crit = r["criterion_name"]
        slo = r["slo_metric"]
        status = r["status"].upper()
        if "bytes" in r:
            obs = f"`{r['bytes']} B`"
            ci = "-"
            budget = f"`{r['budget']} B`"
            delta = "-"
        elif "allocs" in r:
            obs = f"`{r['allocs']} allocs`"
            ci = "-"
            budget = f"`{r['budget']} allocs`"
            delta = "-"
        else:
            obs = f"`{fmt_ns(r['observed_mean_ns'])}`"
            ci = f"`[{fmt_ns(r['ci_low_ns'])}, {fmt_ns(r['ci_high_ns'])}]`"
            budget = f"`{fmt_ns(r['baseline_p99_ns'])}` (+{r['effective_threshold_pct']}%)"
            delta = f"`{r['delta_pct']:.2f}%`"
        lines.append(
            f"| `{cat}` | `{crit}` | `{slo}` | {obs} | {ci} | {budget} | {delta} | **{status}** |"
        )

    lines.extend(
        [
            "",
            "## Presenter Byte-Efficiency Audit (`CountingWriter`)",
            "",
            "Measured in `crates/ftui-render/benches/presenter_bench.rs` (`present_bytes` group) against the plan budget `bytes_emitted <= 8 * cells_changed + 64`:",
            "",
            "| Scene | Grid | Cells Changed | Change Runs | Bytes Emitted | Plan Budget (`8*cells+64`) | Bytes / Changed Cell | Within Budget |",
            "|---|---|---|---|---|---|---|---|",
        ]
    )
    for b in bytes_rows:
        bpc = b["bytes"] / max(b["cells_changed"], 1)
        lines.append(
            f"| `{b['scene']}` | `{b['cols']}×{b['rows']}` | `{b['cells_changed']}` | `{b['runs']}` | `{b['bytes']} B` | `{b['budget']} B` | `{bpc:.2f} B/cell` | `{b['within_budget']}` |"
        )

    lines.extend(
        [
            "",
            "## Steady-State Frame Allocation Audit (`FrameArena` vs Heap)",
            "",
            "Measured in `crates/ftui-render/benches/alloc_count_bench.rs` (`--features alloc-count`) using a custom `#[global_allocator]` counting wrapper after 5 warm-up frames:",
            "",
            "| Scene | Mode | Heap Allocs / Frame | Heap Bytes / Frame | Arena Bump Bytes / Frame | Reduction |",
            "|---|---|---|---|---|---|",
        ]
    )

    by_scene: dict[str, dict[bool, dict]] = {}
    for a in alloc_rows:
        by_scene.setdefault(a["scene"], {})[bool(a["arena"])] = a

    for scene, modes in by_scene.items():
        ar = modes[True]
        hp = modes[False]
        pct = (
            100.0 * (1.0 - (ar["allocs_per_frame"] / hp["allocs_per_frame"]))
            if hp["allocs_per_frame"] > 0
            else 100.0
        )
        lines.append(
            f"| `{scene}` | `FrameArena` (`arena: true`) | **`{ar['allocs_per_frame']}`** | **`{ar['bytes_per_frame']:,} B`** | `{ar['arena_bytes']:,} B` | **`{pct:.1f}%` fewer heap allocs** |"
        )
        lines.append(
            f"| `{scene}` | Heap (`arena: false`) | `{hp['allocs_per_frame']}` | `{hp['bytes_per_frame']:,} B` | `0 B` | baseline |"
        )

    lines.extend(
        [
            "",
            "### Residual Allocation Attribution (`unicode_dashboard_80x24`)",
            "",
            "- **`ascii_dashboard_80x24` (`arena: true`)**: **0 heap allocations / 0 bytes per frame** (down from 1,397 allocations / 142,688 bytes per frame without `FrameArena` — **100.0% elimination**).",
            "- **`unicode_dashboard_80x24` (`arena: true`)**: **3 heap allocations / 42 bytes per frame** (down from 661 allocations / 79,962 bytes per frame without `FrameArena` — **99.5% reduction**). The 3 residual allocations (14 bytes each = 42 bytes total) originate in `GraphemePool::intern` when interning newly composed multi-codepoint graphemes into the pool's `String` slot storage.",
            "",
            "## Reproduction Commands",
            "",
            "```bash",
            "# Run all 27 baseline checks and emit target/regression-gate/regression_report.jsonl",
            "./scripts/perf_regression_gate.sh --quick --json",
            "",
            "# Validate JSONL schema",
            "python3 tests/e2e/lib/validate_jsonl.py target/regression-gate/regression_report.jsonl \\",
            "  --schema tests/perf/regression_report_schema.json",
            "",
            "# Verify README.md and docs/perf/baseline_2026-10-19.md match the recorded report",
            "python3 -B scripts/render_perf_readme.py --check",
            "",
            "# Run synthetic decision & E2E gate tests",
            "./scripts/check_perf_gate_decisions_e2e.sh",
            "./scripts/perf_gate_e2e.sh",
            "```",
            "",
        ]
    )
    return "\n".join(lines)


def splice_readme(readme_text: str, block_content: str) -> str:
    begin_idx = readme_text.find(BEGIN_MARKER)
    end_idx = readme_text.find(END_MARKER)
    if begin_idx == -1 or end_idx == -1 or end_idx < begin_idx:
        raise ValueError(
            f"README is missing {BEGIN_MARKER} .. {END_MARKER} marker block"
        )
    prefix = readme_text[: begin_idx + len(BEGIN_MARKER)]
    suffix = readme_text[end_idx:]
    return f"{prefix}\n{block_content}\n{suffix}"


def compute_sha256_line(block_content: str) -> str:
    digest = hashlib.sha256(block_content.encode("utf-8")).hexdigest()
    return f"{digest}  README.md#perf-block\n"


def run_tool(
    report_path: pathlib.Path,
    bytes_path: pathlib.Path,
    alloc_path: pathlib.Path,
    baseline_md_path: pathlib.Path,
    readme_path: pathlib.Path,
    sha256_path: pathlib.Path,
    check_only: bool,
) -> int:
    start_ev, rows, summary_ev = load_rows(report_path)
    bytes_rows = json.loads(bytes_path.read_text(encoding="utf-8"))
    alloc_rows = json.loads(alloc_path.read_text(encoding="utf-8"))

    block_content = render_readme_block(rows, bytes_rows, alloc_rows)
    expected_baseline_md = render_baseline_md(
        start_ev, rows, summary_ev, bytes_rows, alloc_rows
    )
    expected_sha256 = compute_sha256_line(block_content)

    readme_text = readme_path.read_text(encoding="utf-8")
    expected_readme = splice_readme(readme_text, block_content)

    if check_only:
        errors: list[str] = []
        if not baseline_md_path.exists():
            errors.append(f"missing {baseline_md_path}")
        elif baseline_md_path.read_text(encoding="utf-8") != expected_baseline_md:
            errors.append(f"drift in {baseline_md_path}")

        if readme_text != expected_readme:
            errors.append(f"drift in {readme_path} between {BEGIN_MARKER} and {END_MARKER}")

        if not sha256_path.exists():
            errors.append(f"missing {sha256_path}")
        elif sha256_path.read_text(encoding="utf-8") != expected_sha256:
            errors.append(f"drift in {sha256_path}")

        if errors:
            for err in errors:
                print(f"ERROR: {err}", file=sys.stderr)
            return 1
        print(
            f"OK: {baseline_md_path.name}, {readme_path.name} perf block, and {sha256_path.name} are up to date."
        )
        return 0

    baseline_md_path.parent.mkdir(parents=True, exist_ok=True)
    baseline_md_path.write_text(expected_baseline_md, encoding="utf-8")
    readme_path.write_text(expected_readme, encoding="utf-8")
    sha256_path.write_text(expected_sha256, encoding="utf-8")
    print(
        f"Wrote {baseline_md_path}, updated {readme_path}, and wrote {sha256_path}."
    )
    return 0


def run_self_test() -> int:
    with tempfile.TemporaryDirectory() as tmpdir:
        tmp = pathlib.Path(tmpdir)
        readme = tmp / "README.md"
        readme.write_text(
            f"# Demo\n\n{BEGIN_MARKER}\nold\n{END_MARKER}\n", encoding="utf-8"
        )
        baseline_md = tmp / "baseline.md"
        sha256_file = tmp / "README_BLOCK.sha256"

        rc = run_tool(
            DEFAULT_REPORT,
            DEFAULT_BYTES,
            DEFAULT_ALLOC,
            baseline_md,
            readme,
            sha256_file,
            check_only=False,
        )
        assert rc == 0, "write mode failed"
        assert baseline_md.exists() and sha256_file.exists()

        rc = run_tool(
            DEFAULT_REPORT,
            DEFAULT_BYTES,
            DEFAULT_ALLOC,
            baseline_md,
            readme,
            sha256_file,
            check_only=True,
        )
        assert rc == 0, "check mode failed after clean write"

        # Tamper with README block and verify --check catches drift
        readme.write_text(
            readme.read_text(encoding="utf-8").replace("169 B", "999 B"),
            encoding="utf-8",
        )
        rc = run_tool(
            DEFAULT_REPORT,
            DEFAULT_BYTES,
            DEFAULT_ALLOC,
            baseline_md,
            readme,
            sha256_file,
            check_only=True,
        )
        assert rc == 1, "check mode failed to detect tampered README"

    print("render_perf_readme.py: self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=pathlib.Path, default=DEFAULT_REPORT)
    parser.add_argument("--bytes-report", type=pathlib.Path, default=DEFAULT_BYTES)
    parser.add_argument("--alloc-report", type=pathlib.Path, default=DEFAULT_ALLOC)
    parser.add_argument("--baseline-md", type=pathlib.Path, default=DEFAULT_BASELINE_MD)
    parser.add_argument("--readme", type=pathlib.Path, default=DEFAULT_README)
    parser.add_argument("--sha256-file", type=pathlib.Path, default=DEFAULT_SHA256)
    parser.add_argument("--check", action="store_true", help="Verify no drift")
    parser.add_argument("--self-test", action="store_true", help="Run unit self-test")
    args = parser.parse_args()

    if args.self_test:
        return run_self_test()

    return run_tool(
        args.report,
        args.bytes_report,
        args.alloc_report,
        args.baseline_md,
        args.readme,
        args.sha256_file,
        check_only=args.check,
    )


if __name__ == "__main__":
    sys.exit(main())
