# Telemetry Performance Budget (bd-1z02.11)

Performance budgets and regression gates for the optional OpenTelemetry integration.

## Key Invariants

1. **Zero overhead when disabled**: The disabled path (no OTEL env vars) must be near-zero overhead
2. **Single boolean check**: `is_enabled()` is a simple field read, not env var lookup
3. **Early exit paths**: SDK disabled / exporter=none short-circuit immediately
4. **Redaction is cheap**: All redaction functions are constant-time or O(n) where n is string length

## Performance Budgets

| Operation | Budget | Measured | Status |
|-----------|--------|----------|--------|
| `from_env()` disabled path | < 500ns | ~28ns | ✓ |
| `from_env()` SDK disabled | < 200ns | ~50ns | ✓ |
| `from_env()` enabled (endpoint) | < 2µs | ~152ns | ✓ |
| `from_env()` full config | < 5µs | ~1.1µs | ✓ |
| `is_enabled()` check | < 5ns | ~1ns | ✓ |
| `TraceId::parse()` valid | < 200ns | TBD | - |
| `SpanId::parse()` valid | < 100ns | TBD | - |
| Redaction functions | < 50ns | ~5-20ns | ✓ |
| `contains_sensitive_pattern()` | < 500ns | TBD | - |

## JSONL Performance Log Schema

Performance logs are written in JSONL format for CI integration:

```json
{
  "schema_version": "1.0.0",
  "run_id": "uuid-v4",
  "timestamp": "2026-02-03T07:30:00Z",
  "env": {
    "platform": "linux-x86_64",
    "rust_version": "nightly-2026-01-15",
    "cpu": "AMD EPYC 7763",
    "features": ["telemetry"]
  },
  "benchmarks": [
    {
      "name": "telemetry/config/from_env_disabled",
      "unit": "ns",
      "p50": 28.3,
      "p95": 29.1,
      "p99": 30.2,
      "budget": 500,
      "pass": true
    }
  ],
  "summary": {
    "total": 24,
    "passed": 24,
    "failed": 0,
    "budget_exceeded": []
  }
}
```

### Schema Fields

| Field | Type | Description |
|-------|------|-------------|
| `schema_version` | string | Schema version (semver) |
| `run_id` | string | Unique run identifier (UUID v4) |
| `timestamp` | string | ISO-8601 timestamp |
| `env.platform` | string | OS and architecture |
| `env.rust_version` | string | Rust toolchain version |
| `env.cpu` | string | CPU model |
| `env.features` | string[] | Enabled Cargo features |
| `benchmarks[].name` | string | Benchmark name (group/function) |
| `benchmarks[].unit` | string | Time unit (ns, µs, ms) |
| `benchmarks[].p50` | number | Median time |
| `benchmarks[].p95` | number | 95th percentile |
| `benchmarks[].p99` | number | 99th percentile |
| `benchmarks[].budget` | number | Budget in same unit |
| `benchmarks[].pass` | boolean | Whether budget was met |
| `summary.budget_exceeded` | string[] | Names of failed benchmarks |

## Running Benchmarks

### Quick Test (verify benchmarks work)

```bash
cargo bench -p ftui-runtime --features telemetry --bench telemetry_bench -- --test
```

### Full Benchmark Run

```bash
cargo bench -p ftui-runtime --features telemetry --bench telemetry_bench
```

### Specific Benchmark Group

```bash
# Config parsing
cargo bench -p ftui-runtime --features telemetry --bench telemetry_bench -- "config/"

# ID parsing
cargo bench -p ftui-runtime --features telemetry --bench telemetry_bench -- "id_parsing/"

# Redaction functions
cargo bench -p ftui-runtime --features telemetry --bench telemetry_bench -- "redaction/"

# Validation
cargo bench -p ftui-runtime --features telemetry --bench telemetry_bench -- "validation/"
```

### Budget Enforcement Script

The global budget runner includes telemetry benches and checks:

```bash
./scripts/bench_budget.sh --check-only
```

To re-run benches (including `ftui-runtime:telemetry_bench` with
`--features telemetry`) and emit JSONL:

```bash
./scripts/bench_budget.sh --json
```

This writes two machine-readable ledgers under `target/benchmark-results/`:

- `perf_log.jsonl` — hard gate results (actual vs budget; pass/fail/skip)
- `perf_confidence.jsonl` — confidence evidence per benchmark (CI width, one-step e-value, Bayes factor, expected-loss decision hint) plus run-level summary + hardware provenance

### Confidence Ledger Entry (perf_confidence.jsonl)

```json
{
  "run_id": "20260208T202350-2604979",
  "benchmark": "web/frame_harness_stats/heavy_50pct/80x24",
  "status": "PASS",
  "actual_ns": 10000,
  "budget_ns": 30000,
  "ci_low_ns": 9900,
  "ci_high_ns": 10100,
  "ci_width_ns": 200,
  "relative_ci_width": 0.006667,
  "sigma_ns": 150.0,
  "variance_ns2": 22500.0,
  "posterior_prob_regression": 0.0,
  "e_value": 1.0,
  "bayes_factor_10": 0.196116,
  "decision": "allow",
  "confidence_hint": "pass",
  "loss_matrix": { "false_positive": 1, "false_negative": 5 },
  "hardware": {
    "os": "Linux",
    "arch": "x86_64",
    "cpu_model": "AMD EPYC 7B13",
    "cpu_cores": 16
  }
}
```

At the end of each run, the same file includes a summary event:

```json
{
  "run_id": "20260208T202350-2604979",
  "event": "summary",
  "totals": { "passed": 42, "failed": 1, "panicked": 0, "skipped": 3 },
  "confidence_hints": { "likely_regression": 1, "likely_noise": 0, "uncertain": 0 },
  "loss_matrix": { "false_positive": 1, "false_negative": 5 },
  "hardware": {
    "os": "Linux",
    "arch": "x86_64",
    "cpu_model": "AMD EPYC 7B13",
    "cpu_cores": 16
  }
}
```

### Baseline with Hyperfine

For absolute timing, use hyperfine with a minimal test program:

```bash
hyperfine --warmup 3 'cargo run --release --example telemetry_overhead' \
  --export-json telemetry_baseline.json
```

## Regression Detection

Benchmarks run in CI with the following gates:

1. **Hard failure**: Any benchmark exceeds 2x its budget
2. **Warning**: Any benchmark exceeds 1.5x its budget
3. **Trend alert**: 10% regression from previous run
4. **Confidence evidence (advisory)**: `perf_confidence.jsonl` provides one-sided regression evidence (`posterior_prob_regression`, `e_value`, `bayes_factor_10`) and expected-loss hints.

### Decision Policy

- The hard budget gate remains authoritative for CI pass/fail.
- Confidence evidence is used to triage flaky over-budget cases:
  - `confidence_hint=likely_regression` favors immediate fix/rollback.
  - `confidence_hint=likely_noise` favors rerun + environment variance check.
  - `confidence_hint=uncertain` favors collecting more runs before override.

### CI Integration Example

```yaml
- name: Run telemetry benchmarks
  run: |
    cargo bench -p ftui-runtime --features telemetry \
      --bench telemetry_bench -- --noplot \
      | tee bench_output.txt

- name: Check budgets
  run: |
    # Parse criterion output and verify budgets
    ./scripts/check_telemetry_budgets.sh bench_output.txt

- name: Check budgets (preferred)
  run: |
    ./scripts/bench_budget.sh --check-only
```

## Profiling

### CPU Flamegraph

```bash
cargo flamegraph -p ftui-runtime --features telemetry \
  --bench telemetry_bench -- --bench "from_env_full_config"
```

### Allocation Profile

```bash
DHAT_LOG=telemetry_allocs.txt cargo run --features dhat-heap \
  -p ftui-runtime --example telemetry_overhead
```

## Known Hotspots

1. **KV list parsing**: `parse_kv_list()` allocates for each key=value pair
   - Optimization: Pre-size Vec based on comma count
   - Impact: ~20% reduction for full config path

2. **String comparisons**: Multiple `eq_ignore_ascii_case` calls
   - Already optimized with early returns
   - Further optimization unlikely to yield significant gains

3. **Trace ID parsing**: Hex decoding is the primary cost
   - Uses `from_str_radix` which is well-optimized
   - Alternative: SIMD hex decoding (not worth complexity for 32 bytes)

## Changelog

### v1.0.0 (2026-02-03)
- Initial performance budget documentation
- Benchmark harness with criterion
- JSONL schema for CI integration
