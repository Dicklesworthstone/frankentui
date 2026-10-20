# ADR-002: Presenter Emission Strategy

Status: ACCEPTED

## Context

The Presenter is responsible for transforming a desired Frame into ANSI escape sequences that update the terminal. This involves tracking and managing:

- **SGR state** (bold, italic, colors, etc.)
- **Hyperlink state** (OSC 8)
- **Cursor position**

Getting this wrong causes subtle, hard-to-debug issues:
- Incorrect SGR sequencing leaks style across cells
- Incorrect cursor moves corrupt subsequent renders or user input location
- Incorrectly terminated OSC 8 hyperlinks cause "sticky" links
- Missing resets break muxes and downstream output

This ADR documents the emission strategy based on findings from spike bd-10i.1.2 (Terminal Model Validation).

## Options Considered

### Option A: Reset+Apply Always

Emit SGR 0 (reset) then re-apply all attributes for each style change.

**Pros:**
- Simplest implementation
- Trivially correct: no state tracking bugs possible
- Easy to test and verify
- Safe in all terminal environments

**Cons:**
- More bytes emitted than necessary
- May be slower on bandwidth-constrained terminals (SSH, slow connections)

### Option B: Incremental SGR Diffs

Track previous style state, emit only the attributes that changed.

**Pros:**
- Fewer bytes emitted
- Potentially faster on slow terminals/connections
- More "efficient" output

**Cons:**
- Complex state tracking
- Risk of "dangling attributes" bugs (attributes bleeding between cells)
- Difficult to verify correctness
- Edge cases around partial style changes

### Option C: Hybrid (Future Optimization)

Reset on hard cases (major style transitions), incremental on small diffs.

**Pros:**
- Best of both approaches
- Efficient for common cases
- Safe fallback for edge cases

**Cons:**
- Needs heuristics to decide when to reset vs diff
- Requires benchmarks to validate benefit
- More complex implementation

## Decision

**v1 Default: Reset+Apply (Option A)**

All style changes emit `SGR 0` followed by the complete style specification. This applies to:
- Foreground and background colors
- Text attributes (bold, italic, underline, etc.)
- Hyperlinks (always emit complete OSC 8 open/close)

**Incremental emission (Options B/C) deferred** until:
1. Terminal model tests provide comprehensive coverage
2. Benchmarks show meaningful benefit for real workloads
3. PTY tests cover all edge cases

## Rationale

Correctness is more important than optimization at this stage:

1. **Trivially correct**: Reset+apply cannot have dangling attribute bugs
2. **Easy to verify**: Terminal model tests confirm state after each present
3. **Good enough for most workloads**: Modern terminals are fast
4. **Foundation for optimization**: Clean baseline makes incremental safe to add later
5. **No maintenance burden**: Simple code, fewer edge cases

The terminal model (bd-10i.1.2) provides the test infrastructure to eventually add incremental emission safely. When benchmarks show meaningful benefit, we can add it behind a feature flag without risking correctness.

## Consequences

### Positive
- Simple, correct implementation
- No "leaky style" bugs
- Easy to test and maintain
- Clear path to future optimization

### Negative
- Higher byte output than theoretically minimal
- May be slower on very constrained connections

### Trade-offs Accepted
- We accept higher byte count for correctness
- We defer optimization until we have evidence it matters

## Test Plan

The Terminal Model (from bd-10i.1.2) enables comprehensive testing:

1. **Style isolation tests**: Verify style state cannot leak across runs
2. **Hyperlink balance tests**: OSC 8 links cannot remain open after present/exit
3. **Property tests**: Random buffer → present → model apply → correct grid
4. **Byte budget tests**: Measure bytes/frame for representative workloads
5. **Regression tests**: Track byte output in CI

## Implementation Notes

### Presenter Contract

```rust
impl Presenter {
    /// Emit ANSI sequences to transform terminal to match `frame`.
    /// Uses reset+apply strategy for all style changes.
    pub fn present(&mut self, frame: &Frame, writer: &mut impl Write) -> io::Result<()> {
        // For each cell with style change:
        // 1. Emit SGR 0
        // 2. Emit current style (colors + attributes)
        // 3. Emit character
    }
}
```

### Future Optimization Path

When adding incremental emission:
1. Add `PresenterConfig::emission_strategy: EmissionStrategy`
2. Default to `EmissionStrategy::ResetApply`
3. Add `EmissionStrategy::Incremental` behind feature flag
4. Verify with terminal model tests before enabling by default

## Related

- **bd-10i.1.2**: Terminal Model spike (provides test infrastructure)
- **bd-10i.4.3**: Implement Presenter with state-tracked ANSI emission
- **ADR-001**: Inline Mode Strategy (complementary decision)
