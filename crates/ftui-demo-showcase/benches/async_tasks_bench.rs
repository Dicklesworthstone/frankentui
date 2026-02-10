//! Benchmarks for Async Task Manager screen (bd-13pq.2)
//!
//! Performance Regression Tests for task scheduling and management.
//!
//! Run with: cargo bench -p ftui-demo-showcase --bench async_tasks_bench
//!
//! Performance budgets (per bd-13pq.2):
//! - Empty render: < 50µs
//! - 50 tasks render: < 200µs
//! - 100 tasks render: < 500µs
//! - MAX_TASKS (100) render: < 2ms
//! - Policy switch: < 10µs
//! - Tick advancement: < 100µs per tick
//! - Task spawn: < 20µs
//! - Task cancel: < 10µs

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::screens::{Screen, async_tasks::AsyncTaskManager};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use std::hint::black_box;

// =============================================================================
// Render Benchmarks: Various Task Counts
// =============================================================================

fn bench_async_tasks_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_tasks/render");

    // Empty manager (no tasks)
    group.bench_function("empty_120x40", |b| {
        let mgr = create_empty_manager();
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        b.iter(|| {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
            black_box(&frame);
        })
    });

    // Default manager (3 initial tasks)
    group.bench_function("initial_120x40", |b| {
        let mgr = AsyncTaskManager::new();
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        b.iter(|| {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
            black_box(&frame);
        })
    });

    // 50 tasks
    group.throughput(Throughput::Elements(50));
    group.bench_function("50_tasks_120x40", |b| {
        let mgr = create_manager_with_tasks(50);
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        b.iter(|| {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
            black_box(&frame);
        })
    });

    // 100 tasks (MAX_TASKS)
    group.throughput(Throughput::Elements(100));
    group.bench_function("100_tasks_120x40", |b| {
        let mgr = create_manager_with_tasks(100);
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        b.iter(|| {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
            black_box(&frame);
        })
    });

    // 100 tasks on small screen (80x24)
    group.throughput(Throughput::Elements(100));
    group.bench_function("100_tasks_80x24", |b| {
        let mgr = create_manager_with_tasks(100);
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 80, 24);

        b.iter(|| {
            let mut frame = Frame::new(80, 24, &mut pool);
            mgr.view(&mut frame, area);
            black_box(&frame);
        })
    });

    // Mixed state tasks (some running, some completed, some failed)
    group.bench_function("mixed_states_120x40", |b| {
        let mgr = create_manager_with_mixed_states(50);
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        b.iter(|| {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
            black_box(&frame);
        })
    });

    group.finish();
}

// =============================================================================
// Scheduler Benchmarks
// =============================================================================

fn bench_async_tasks_scheduler(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_tasks/scheduler");

    // Tick with few tasks
    group.bench_function("tick_10_tasks", |b| {
        let mut mgr = create_manager_with_tasks(10);
        let mut tick = 0u64;

        b.iter(|| {
            tick += 1;
            mgr.tick(tick);
            black_box(&mgr);
        })
    });

    // Tick with many tasks
    group.bench_function("tick_100_tasks", |b| {
        let mut mgr = create_manager_with_tasks(100);
        let mut tick = 0u64;

        b.iter(|| {
            tick += 1;
            mgr.tick(tick);
            black_box(&mgr);
        })
    });

    // Policy cycling
    group.bench_function("cycle_policy", |b| {
        let mut mgr = create_manager_with_tasks(50);

        b.iter(|| {
            mgr.update(&press(KeyCode::Char('s')));
            black_box(&mgr);
        })
    });

    // Full scheduler run with many queued tasks
    group.bench_function("schedule_50_queued", |b| {
        b.iter_batched(
            || create_manager_with_queued_tasks(50),
            |mut mgr| {
                mgr.tick(1);
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

// =============================================================================
// Task Operation Benchmarks
// =============================================================================

fn bench_async_tasks_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_tasks/operations");

    // Task spawn
    group.bench_function("spawn_task", |b| {
        let mut mgr = create_empty_manager();

        b.iter(|| {
            mgr.update(&press(KeyCode::Char('n')));
            black_box(&mgr);
        })
    });

    // Task cancel
    group.bench_function("cancel_task", |b| {
        b.iter_batched(
            || {
                let mut mgr = create_manager_with_tasks(50);
                mgr.update(&press(KeyCode::Down)); // Select a task
                mgr
            },
            |mut mgr| {
                mgr.update(&press(KeyCode::Char('c')));
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Task retry
    group.bench_function("retry_task", |b| {
        b.iter_batched(
            create_manager_with_failed_task,
            |mut mgr| {
                mgr.update(&press(KeyCode::Char('r')));
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Navigation (up/down)
    group.bench_function("navigate_down", |b| {
        let mut mgr = create_manager_with_tasks(50);

        b.iter(|| {
            mgr.update(&press(KeyCode::Down));
            black_box(&mgr);
        })
    });

    group.bench_function("navigate_up", |b| {
        let mut mgr = create_manager_with_tasks(50);
        // Start at end
        for _ in 0..49 {
            mgr.update(&press(KeyCode::Down));
        }

        b.iter(|| {
            mgr.update(&press(KeyCode::Up));
            black_box(&mgr);
        })
    });

    group.finish();
}

// =============================================================================
// Stress Test: Many Ticks
// =============================================================================

fn bench_async_tasks_stress(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_tasks/stress");

    // 100 ticks with 100 tasks
    group.bench_function("100_ticks_100_tasks", |b| {
        b.iter_batched(
            || create_manager_with_tasks(100),
            |mut mgr| {
                for tick in 1..=100 {
                    mgr.tick(tick);
                }
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Continuous task spawning during ticks
    group.bench_function("spawn_during_ticks", |b| {
        b.iter_batched(
            || create_manager_with_tasks(20),
            |mut mgr| {
                for tick in 1..=50 {
                    mgr.tick(tick);
                    if tick % 5 == 0 {
                        mgr.update(&press(KeyCode::Char('n')));
                    }
                }
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Render after many ticks (with completed/failed tasks)
    group.bench_function("render_after_100_ticks", |b| {
        let mut mgr = create_manager_with_tasks(50);
        for tick in 1..=100 {
            mgr.tick(tick);
        }
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        b.iter(|| {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
            black_box(&frame);
        })
    });

    group.finish();
}

// =============================================================================
// Cancellation Stress Tests (bd-13pq.2)
// =============================================================================

fn bench_async_tasks_cancellation(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_tasks/cancellation");

    // Cancel single task in large queue (worst-case lookup)
    group.bench_function("cancel_in_100_tasks", |b| {
        b.iter_batched(
            || {
                let mut mgr = create_manager_with_tasks(100);
                // Navigate to middle of list
                for _ in 0..50 {
                    mgr.update(&press(KeyCode::Down));
                }
                mgr
            },
            |mut mgr| {
                mgr.update(&press(KeyCode::Char('c')));
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Rapid cancellation of many tasks sequentially
    group.bench_function("cancel_50_sequential", |b| {
        b.iter_batched(
            || create_manager_with_tasks(100),
            |mut mgr| {
                for _ in 0..50 {
                    mgr.update(&press(KeyCode::Down));
                    mgr.update(&press(KeyCode::Char('c')));
                }
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Cancel during active scheduling (interleaved with ticks)
    group.bench_function("cancel_during_ticks", |b| {
        b.iter_batched(
            || create_manager_with_tasks(50),
            |mut mgr| {
                for tick in 1..=50 {
                    mgr.tick(tick);
                    if tick % 3 == 0 {
                        mgr.update(&press(KeyCode::Down));
                        mgr.update(&press(KeyCode::Char('c')));
                    }
                }
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Cancel and retry cycle (tests state transition overhead)
    group.bench_function("cancel_retry_cycle", |b| {
        b.iter_batched(
            || {
                let mut mgr = create_manager_with_tasks(20);
                // Run some ticks to get mixed states
                for tick in 1..=30 {
                    mgr.tick(tick);
                }
                mgr
            },
            |mut mgr| {
                for _ in 0..10 {
                    mgr.update(&press(KeyCode::Down));
                    mgr.update(&press(KeyCode::Char('c'))); // Cancel
                    mgr.update(&press(KeyCode::Char('r'))); // Retry
                }
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

// =============================================================================
// Latency Percentile Tests (bd-13pq.2)
// =============================================================================

fn bench_async_tasks_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_tasks/latency");
    group.sample_size(200); // More samples for better percentile accuracy

    // Tick latency with various loads
    for task_count in [10, 50, 100] {
        group.bench_function(format!("tick_p99_{}_tasks", task_count).as_str(), |b| {
            let mut mgr = create_manager_with_tasks(task_count);
            let mut tick = 0u64;

            b.iter(|| {
                tick += 1;
                mgr.tick(tick);
                black_box(&mgr);
            })
        });
    }

    // Render latency with various loads
    for task_count in [10, 50, 100] {
        group.bench_function(format!("render_p99_{}_tasks", task_count).as_str(), |b| {
            let mgr = create_manager_with_tasks(task_count);
            let mut pool = GraphemePool::new();
            let area = Rect::new(0, 0, 120, 40);

            b.iter(|| {
                let mut frame = Frame::new(120, 40, &mut pool);
                mgr.view(&mut frame, area);
                black_box(&frame);
            })
        });
    }

    // Cancellation latency under load
    group.bench_function("cancel_p99_under_load", |b| {
        b.iter_batched(
            || {
                let mut mgr = create_manager_with_tasks(100);
                // Start some tasks running
                for tick in 1..=10 {
                    mgr.tick(tick);
                }
                mgr.update(&press(KeyCode::Down));
                mgr
            },
            |mut mgr| {
                mgr.update(&press(KeyCode::Char('c')));
                black_box(&mgr);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

// =============================================================================
// Helpers
// =============================================================================

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

fn create_empty_manager() -> AsyncTaskManager {
    // Note: AsyncTaskManager::new() creates 3 initial tasks, so this isn't truly "empty"
    // but serves as baseline for benchmarking
    AsyncTaskManager::new()
}

fn create_manager_with_tasks(count: usize) -> AsyncTaskManager {
    let mut mgr = AsyncTaskManager::new();
    // Spawn additional tasks beyond the 3 initial ones
    let additional = count.saturating_sub(3);
    for _ in 0..additional {
        mgr.update(&press(KeyCode::Char('n')));
    }
    mgr
}

fn create_manager_with_queued_tasks(count: usize) -> AsyncTaskManager {
    // Create manager with all tasks queued (not yet scheduled)
    let mut mgr = AsyncTaskManager::new();
    let additional = count.saturating_sub(3);
    for _ in 0..additional {
        mgr.update(&press(KeyCode::Char('n')));
    }
    // Don't tick so tasks remain queued
    mgr
}

fn create_manager_with_mixed_states(count: usize) -> AsyncTaskManager {
    let mut mgr = create_manager_with_tasks(count);
    // Run some ticks to get mixed states
    for tick in 1..=50 {
        mgr.tick(tick);
    }
    // Cancel some tasks
    for _ in 0..5 {
        mgr.update(&press(KeyCode::Down));
        mgr.update(&press(KeyCode::Char('c')));
    }
    mgr
}

fn create_manager_with_failed_task() -> AsyncTaskManager {
    let mut mgr = AsyncTaskManager::new();
    // Run ticks until at least one task fails (id % 20 == 7 fails)
    // We need task id 7 to complete
    // Add tasks to get to id 7
    for _ in 0..4 {
        mgr.update(&press(KeyCode::Char('n'))); // ids 4, 5, 6, 7
    }
    // Run scheduler and advance to completion
    for tick in 1..=200 {
        mgr.tick(tick);
    }
    // Select the failed task (task 7 should have failed)
    mgr
}

// =============================================================================
// Criterion Configuration
// =============================================================================

criterion_group!(
    benches,
    bench_async_tasks_render,
    bench_async_tasks_scheduler,
    bench_async_tasks_operations,
    bench_async_tasks_stress,
    bench_async_tasks_cancellation,
    bench_async_tasks_latency,
);

criterion_main!(benches);

// =============================================================================
// Regression Tests (Quick Sanity Checks)
// =============================================================================

#[cfg(test)]
mod regression_tests {
    use super::*;
    use std::time::Instant;

    const COVERAGE_BUDGET_MULTIPLIER: u128 = 5;

    fn is_coverage_run() -> bool {
        std::env::var("LLVM_PROFILE_FILE").is_ok() || std::env::var("CARGO_LLVM_COV").is_ok()
    }

    fn budget_us(default_us: u128) -> u128 {
        if is_coverage_run() {
            default_us.saturating_mul(COVERAGE_BUDGET_MULTIPLIER)
        } else {
            default_us
        }
    }

    fn budget_ms(default_ms: u128) -> u128 {
        if is_coverage_run() {
            default_ms.saturating_mul(COVERAGE_BUDGET_MULTIPLIER)
        } else {
            default_ms
        }
    }

    /// Budget: Empty render < 50µs (with generous margin for CI)
    #[test]
    fn budget_empty_render() {
        let mgr = AsyncTaskManager::new();
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        // Warmup: prime caches and page tables before timing.
        {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
        }

        let start = Instant::now();
        for _ in 0..100 {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
        }
        let elapsed = start.elapsed() / 100;

        // Allow 1ms for CI variability (budget is 50µs), relaxed further under coverage runs.
        let limit_us = budget_us(1_000);
        assert!(
            elapsed.as_micros() < limit_us,
            "Empty render took {:?} (budget: 50µs, limit: {}µs, coverage={})",
            elapsed,
            limit_us,
            is_coverage_run()
        );
    }

    /// Budget: 100 tasks render < 2ms (with generous margin for CI)
    #[test]
    fn budget_100_tasks_render() {
        let mgr = create_manager_with_tasks(100);
        let mut pool = GraphemePool::new();
        let area = Rect::new(0, 0, 120, 40);

        let start = Instant::now();
        for _ in 0..10 {
            let mut frame = Frame::new(120, 40, &mut pool);
            mgr.view(&mut frame, area);
        }
        let elapsed = start.elapsed() / 10;

        // Allow 10ms for CI variability (budget is 2ms), relaxed further under coverage runs.
        let limit_ms = budget_ms(10);
        assert!(
            elapsed.as_millis() < limit_ms,
            "100 tasks render took {:?} (budget: 2ms, limit: {}ms, coverage={})",
            elapsed,
            limit_ms,
            is_coverage_run()
        );
    }

    /// Budget: Tick operation < 100µs
    #[test]
    fn budget_tick_operation() {
        let mut mgr = create_manager_with_tasks(100);

        let start = Instant::now();
        for tick in 1..=100 {
            mgr.tick(tick);
        }
        let elapsed = start.elapsed() / 100;

        // Allow 1ms for CI variability (budget is 100µs per tick), relaxed further under coverage runs.
        let limit_us = budget_us(1_000);
        assert!(
            elapsed.as_micros() < limit_us,
            "Tick operation took {:?} (budget: 100µs, limit: {}µs, coverage={})",
            elapsed,
            limit_us,
            is_coverage_run()
        );
    }

    /// Budget: Policy switch < 10µs
    #[test]
    fn budget_policy_switch() {
        let mut mgr = create_manager_with_tasks(50);

        let start = Instant::now();
        for _ in 0..100 {
            mgr.update(&press(KeyCode::Char('s')));
        }
        let elapsed = start.elapsed() / 100;

        // Allow 500µs for CI variability (budget is 10µs), relaxed further under coverage runs.
        let limit_us = budget_us(500);
        assert!(
            elapsed.as_micros() < limit_us,
            "Policy switch took {:?} (budget: 10µs, limit: {}µs, coverage={})",
            elapsed,
            limit_us,
            is_coverage_run()
        );
    }

    /// Budget: Task spawn < 20µs
    #[test]
    fn budget_task_spawn() {
        let mut mgr = create_empty_manager();

        let start = Instant::now();
        for _ in 0..100 {
            mgr.update(&press(KeyCode::Char('n')));
        }
        let elapsed = start.elapsed() / 100;

        // Allow 500µs for CI variability (budget is 20µs), relaxed further under coverage runs.
        let limit_us = budget_us(500);
        assert!(
            elapsed.as_micros() < limit_us,
            "Task spawn took {:?} (budget: 20µs, limit: {}µs, coverage={})",
            elapsed,
            limit_us,
            is_coverage_run()
        );
    }

    /// Stress test: 100 ticks with 100 tasks completes in reasonable time
    #[test]
    fn stress_100_ticks_100_tasks() {
        let mut mgr = create_manager_with_tasks(100);

        let start = Instant::now();
        for tick in 1..=100 {
            mgr.tick(tick);
        }
        let elapsed = start.elapsed();

        // Should complete in under 100ms (relaxed under coverage runs).
        let limit_ms = budget_ms(100);
        assert!(
            elapsed.as_millis() < limit_ms,
            "100 ticks with 100 tasks took {:?} (limit: {}ms, coverage={})",
            elapsed,
            limit_ms,
            is_coverage_run()
        );
    }

    /// Budget: Task cancel < 10µs (bd-13pq.2)
    #[test]
    fn budget_task_cancel() {
        let mut mgr = create_manager_with_tasks(50);
        mgr.update(&press(KeyCode::Down)); // Select a task

        let start = Instant::now();
        for _ in 0..100 {
            // Reset selection and cancel
            mgr.update(&press(KeyCode::Home));
            mgr.update(&press(KeyCode::Down));
            mgr.update(&press(KeyCode::Char('c')));
        }
        let elapsed = start.elapsed() / 100;

        // Allow 500µs for CI variability (budget is 10µs per cancel), relaxed further under coverage runs.
        let limit_us = budget_us(500);
        assert!(
            elapsed.as_micros() < limit_us,
            "Task cancel took {:?} (budget: 10µs, limit: {}µs, coverage={})",
            elapsed,
            limit_us,
            is_coverage_run()
        );
    }

    /// Stress: Rapid cancellation of 50 tasks sequentially (bd-13pq.2)
    #[test]
    fn stress_cancel_50_sequential() {
        let mut mgr = create_manager_with_tasks(100);

        let start = Instant::now();
        for _ in 0..50 {
            mgr.update(&press(KeyCode::Down));
            mgr.update(&press(KeyCode::Char('c')));
        }
        let elapsed = start.elapsed();

        // Should complete in under 50ms (relaxed under coverage runs).
        let limit_ms = budget_ms(50);
        assert!(
            elapsed.as_millis() < limit_ms,
            "50 sequential cancels took {:?} (limit: {}ms, coverage={})",
            elapsed,
            limit_ms,
            is_coverage_run()
        );
    }

    /// Stress: Interleaved cancel with ticks (bd-13pq.2)
    #[test]
    fn stress_cancel_during_ticks() {
        let mut mgr = create_manager_with_tasks(50);

        let start = Instant::now();
        for tick in 1..=50 {
            mgr.tick(tick);
            if tick % 3 == 0 {
                mgr.update(&press(KeyCode::Down));
                mgr.update(&press(KeyCode::Char('c')));
            }
        }
        let elapsed = start.elapsed();

        // Should complete in under 100ms (relaxed under coverage runs).
        let limit_ms = budget_ms(100);
        assert!(
            elapsed.as_millis() < limit_ms,
            "Cancel during ticks took {:?} (limit: {}ms, coverage={})",
            elapsed,
            limit_ms,
            is_coverage_run()
        );
    }
}
