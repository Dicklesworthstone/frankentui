#![forbid(unsafe_code)]

//! Global-allocator instrumentation helper for G25 allocation benchmarks and tests
//! (`bd-g00-root-epic-ewths.31.2`, `bd-g00-root-epic-ewths.31.5`).
//!
//! Wraps `std::alloc::System` via `stats_alloc::StatsAlloc` using 100% safe Rust.

use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};
use std::alloc::System;
use std::hint::black_box;

#[global_allocator]
pub static GLOBAL_ALLOC: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Measured heap allocation delta across a single region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocDelta {
    pub allocations: usize,
    pub deallocations: usize,
    pub reallocations: usize,
    pub bytes_allocated: usize,
    pub bytes_deallocated: usize,
}

impl AllocDelta {
    #[inline]
    #[must_use]
    pub fn from_stats(stats: Stats) -> Self {
        Self {
            allocations: stats.allocations,
            deallocations: stats.deallocations,
            reallocations: stats.reallocations,
            bytes_allocated: stats.bytes_allocated,
            bytes_deallocated: stats.bytes_deallocated,
        }
    }

    /// Total heap allocation operations (`allocations + reallocations`).
    #[inline]
    #[must_use]
    pub const fn total_alloc_ops(&self) -> usize {
        self.allocations.saturating_add(self.reallocations)
    }
}

/// Measure heap allocations performed by `f` on the current thread/process.
#[inline]
pub fn measure_allocations<F, R>(f: F) -> (R, AllocDelta)
where
    F: FnOnce() -> R,
{
    let region = Region::new(GLOBAL_ALLOC);
    let result = f();
    let stats = region.change();
    (result, AllocDelta::from_stats(stats))
}

/// Self-check verifying that `Box::new` increments both allocation and deallocation
/// counters by 1 (`bd-g00-root-epic-ewths.31.5`).
pub fn assert_counter_counts_box_alloc_and_free() {
    let (_, delta) = measure_allocations(|| {
        let boxed = Box::new(black_box(0xDEAD_BEEF_u64));
        let val = **black_box(&boxed);
        drop(boxed);
        val
    });
    assert_eq!(
        delta.allocations, 1,
        "one Box::new must record exactly 1 allocation, got {delta:?}"
    );
    assert_eq!(
        delta.deallocations, 1,
        "dropping one Box<u64> must record exactly 1 deallocation, got {delta:?}"
    );
    assert!(
        delta.bytes_allocated >= std::mem::size_of::<u64>(),
        "bytes_allocated must be at least 8 bytes, got {delta:?}"
    );
}

/// Self-check verifying that stack-only arithmetic records zero heap allocations
/// (`bd-g00-root-epic-ewths.31.5`).
pub fn assert_counter_is_zero_for_stack_only_work() {
    let (sum, delta) = measure_allocations(|| {
        let mut acc = [0u64; 32];
        for (i, slot) in acc.iter_mut().enumerate() {
            *slot = (i as u64).wrapping_mul(17).wrapping_add(3);
        }
        black_box(acc.iter().copied().sum::<u64>())
    });
    assert!(sum > 0);
    assert_eq!(
        delta.total_alloc_ops(),
        0,
        "stack-only work must perform 0 heap allocations, got {delta:?}"
    );
    assert_eq!(
        delta.bytes_allocated, 0,
        "stack-only work must allocate 0 heap bytes, got {delta:?}"
    );
}
