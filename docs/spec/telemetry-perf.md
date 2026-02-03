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
