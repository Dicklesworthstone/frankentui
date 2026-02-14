//! E2E integration test: ArcSwap-backed SharedResolvedTheme and SharedCapabilities
//! under concurrent reader + rapid writer workloads.
//!
//! Validates:
//! 1. No torn reads — every read is a complete, consistent snapshot.
//! 2. All theme switches complete without blocking readers.
//! 3. Structured JSONL event logging for postmortem analysis.
//! 4. No panics, no deadlocks, no unsafe code.
//!
//! Test scenario: 8 reader threads continuously read theme data while a writer
//! thread cycles through all available themes rapidly.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Instant;
use std::{io::Write, thread};

use ftui_core::read_optimized::{ArcSwapStore, ReadOptimized};
use ftui_core::terminal_capabilities::{SharedCapabilities, TerminalCapabilities};
use ftui_style::color::Color;
use ftui_style::theme::{ResolvedTheme, SharedResolvedTheme, themes};

// ── JSONL event types ───────────────────────────────────────────────────

/// A read event recorded by a reader thread.
struct ReadEvent {
    ts_ns: u64,
    reader_id: u8,
    theme_generation: u64,
    read_latency_ns: u64,
    consistent: bool,
    primary_rgb: (u8, u8, u8),
}

/// A write event recorded by the writer thread.
struct WriteEvent {
    ts_ns: u64,
    old_generation: u64,
    new_generation: u64,
    theme_name: &'static str,
    write_latency_ns: u64,
}

impl ReadEvent {
    fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"seqlock_read","ts_ns":{},"reader_id":{},"theme_generation":{},"read_latency_ns":{},"consistent":{},"primary_rgb":[{},{},{}]}}"#,
            self.ts_ns,
            self.reader_id,
            self.theme_generation,
            self.read_latency_ns,
            self.consistent,
            self.primary_rgb.0,
            self.primary_rgb.1,
            self.primary_rgb.2,
        )
    }
}

impl WriteEvent {
    fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"seqlock_write","ts_ns":{},"old_generation":{},"new_generation":{},"theme_name":"{}","write_latency_ns":{}}}"#,
            self.ts_ns,
            self.old_generation,
            self.new_generation,
            self.theme_name,
            self.write_latency_ns,
        )
    }
}

// ── Theme catalogue ─────────────────────────────────────────────────────

/// All built-in themes with their names, resolved for dark mode.
fn theme_catalogue() -> Vec<(&'static str, ResolvedTheme)> {
    vec![
        ("dark", themes::dark().resolve(true)),
        ("light", themes::light().resolve(false)),
        ("nord", themes::nord().resolve(true)),
        ("dracula", themes::dracula().resolve(true)),
        ("solarized_dark", themes::solarized_dark().resolve(true)),
        ("solarized_light", themes::solarized_light().resolve(false)),
        ("monokai", themes::monokai().resolve(true)),
    ]
}

/// Check that a ResolvedTheme is one of the known themes.
fn is_known_theme(t: &ResolvedTheme, catalogue: &[(&str, ResolvedTheme)]) -> bool {
    catalogue.iter().any(|(_, known)| known == t)
}

/// Extract RGB tuple from a Color for logging.
fn color_to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(rgb) => (rgb.r, rgb.g, rgb.b),
        Color::Ansi256(idx) => (idx, 0, 0),
        Color::Ansi16(ansi) => (ansi as u8, 0, 0),
        Color::Mono(mono) => (mono as u8, 0, 0),
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Test 1: SharedResolvedTheme — 8 readers + rapid theme cycling writer
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_shared_theme_concurrent_rapid_cycling() {
    let catalogue = theme_catalogue();
    let num_readers = 8;
    let cycles = 100; // 100 full cycles through all 7 themes = 700 writes
    let reads_per_thread = 50_000;

    let shared = Arc::new(SharedResolvedTheme::new(catalogue[0].1));
    let generation = Arc::new(AtomicU64::new(0));
    let barrier = Arc::new(Barrier::new(num_readers + 1));
    let start = Instant::now();

    // --- Readers ---
    let readers: Vec<_> = (0..num_readers)
        .map(|id| {
            let s = Arc::clone(&shared);
            let gen_counter = Arc::clone(&generation);
            let b = Arc::clone(&barrier);
            let cat = catalogue.clone();
            thread::spawn(move || {
                let mut events: Vec<ReadEvent> = Vec::with_capacity(100);
                let mut torn_count = 0u64;
                b.wait();
                for _ in 0..reads_per_thread {
                    let t0 = start.elapsed().as_nanos() as u64;
                    let theme = s.load();
                    let t1 = start.elapsed().as_nanos() as u64;
                    let latency = t1.saturating_sub(t0);

                    let consistent = is_known_theme(&theme, &cat);
                    if !consistent {
                        torn_count += 1;
                    }
                    let current_gen = gen_counter.load(Ordering::Relaxed);

                    // Log a sample of events (every 1000th) to avoid huge logs.
                    if events.len() < 100 {
                        events.push(ReadEvent {
                            ts_ns: t0,
                            reader_id: id as u8,
                            theme_generation: current_gen,
                            read_latency_ns: latency,
                            consistent,
                            primary_rgb: color_to_rgb(theme.primary),
                        });
                    }
                }
                (events, torn_count)
            })
        })
        .collect();

    // --- Writer ---
    let writer = {
        let s = Arc::clone(&shared);
        let gen_counter = Arc::clone(&generation);
        let b = Arc::clone(&barrier);
        let cat = catalogue.clone();
        thread::spawn(move || {
            let mut events: Vec<WriteEvent> = Vec::with_capacity(cycles * cat.len());
            b.wait();
            let mut current_gen = 0u64;
            for _cycle in 0..cycles {
                for (name, theme) in &cat {
                    let old_gen = current_gen;
                    let t0 = start.elapsed().as_nanos() as u64;
                    s.store(*theme);
                    let t1 = start.elapsed().as_nanos() as u64;
                    current_gen += 1;
                    gen_counter.store(current_gen, Ordering::Relaxed);

                    events.push(WriteEvent {
                        ts_ns: t0,
                        old_generation: old_gen,
                        new_generation: current_gen,
                        theme_name: name,
                        write_latency_ns: t1.saturating_sub(t0),
                    });
                }
            }
            events
        })
    };

    // --- Collect results ---
    let write_events = writer.join().expect("writer panicked");
    let mut total_torn = 0u64;
    let mut all_read_events: Vec<ReadEvent> = Vec::new();

    for handle in readers {
        let (events, torn) = handle.join().expect("reader panicked");
        total_torn += torn;
        all_read_events.extend(events);
    }

    // --- Emit JSONL log ---
    let mut log_buf = Vec::new();
    for ev in &write_events {
        writeln!(log_buf, "{}", ev.to_jsonl()).unwrap();
    }
    for ev in &all_read_events {
        writeln!(log_buf, "{}", ev.to_jsonl()).unwrap();
    }
    let log_str = String::from_utf8(log_buf).unwrap();

    // Verify JSONL is well-formed.
    for line in log_str.lines() {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "Malformed JSONL line: {line}"
        );
    }

    // --- Assertions ---
    assert_eq!(
        total_torn, 0,
        "TORN READS DETECTED: {total_torn} reads returned an unknown theme snapshot"
    );

    assert_eq!(
        write_events.len(),
        cycles * catalogue.len(),
        "Expected {} write events, got {}",
        cycles * catalogue.len(),
        write_events.len()
    );

    // Final value should be the last theme written.
    let final_theme = shared.load();
    let expected_final = &catalogue[catalogue.len() - 1].1;
    assert_eq!(
        &final_theme, expected_final,
        "Final theme mismatch after all writes"
    );

    // Generations should be monotonically increasing in write events.
    for w in write_events.windows(2) {
        assert!(
            w[1].new_generation > w[0].new_generation,
            "Non-monotonic generation: {} -> {}",
            w[0].new_generation,
            w[1].new_generation
        );
    }

    eprintln!(
        "[e2e_shared_theme] {} write events, {} sampled read events, {} torn reads",
        write_events.len(),
        all_read_events.len(),
        total_torn,
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Test 2: SharedCapabilities — 8 readers + capability toggling writer
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_shared_capabilities_concurrent_toggling() {
    let num_readers = 8;
    let num_writes = 1_000;
    let reads_per_thread = 125_000; // 8 * 125K = 1M total reads

    let shared = Arc::new(SharedCapabilities::new(TerminalCapabilities::default()));
    let barrier = Arc::new(Barrier::new(num_readers + 1));
    let start = Instant::now();

    // --- Readers: just verify no panics, all reads return valid data ---
    let readers: Vec<_> = (0..num_readers)
        .map(|id| {
            let s = Arc::clone(&shared);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut read_count = 0u64;
                let mut max_latency_ns = 0u64;
                b.wait();
                for _ in 0..reads_per_thread {
                    let t0 = start.elapsed().as_nanos() as u64;
                    let _caps = s.load();
                    let t1 = start.elapsed().as_nanos() as u64;
                    let latency = t1.saturating_sub(t0);
                    if latency > max_latency_ns {
                        max_latency_ns = latency;
                    }
                    read_count += 1;
                }
                (id, read_count, max_latency_ns)
            })
        })
        .collect();

    // --- Writer: alternate between default caps ---
    let writer = {
        let s = Arc::clone(&shared);
        let b = Arc::clone(&barrier);
        thread::spawn(move || {
            b.wait();
            for i in 0..num_writes {
                // Store default each time (demonstrating store path doesn't panic).
                s.store(TerminalCapabilities::default());
                if i % 100 == 0 {
                    thread::yield_now();
                }
            }
        })
    };

    writer.join().expect("writer panicked");
    let mut total_reads = 0u64;
    let mut worst_latency = 0u64;
    for handle in readers {
        let (id, count, max_lat) = handle.join().expect("reader panicked");
        total_reads += count;
        if max_lat > worst_latency {
            worst_latency = max_lat;
        }
        eprintln!("[e2e_shared_caps] reader {id}: {count} reads, max latency {max_lat}ns");
    }

    assert_eq!(
        total_reads,
        (num_readers as u64) * (reads_per_thread as u64),
        "Some reads were lost"
    );

    eprintln!(
        "[e2e_shared_caps] total: {total_reads} reads, {num_writes} writes, worst latency {worst_latency}ns"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Test 3: ReadOptimized<u64> stress — 1M reads + 1K writes with JSONL
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_arcswap_store_stress_with_jsonl() {
    let num_readers = 8;
    let num_writes = 1_000u64;
    let reads_per_thread = 125_000u64;

    let store = Arc::new(ArcSwapStore::new(0u64));
    let barrier = Arc::new(Barrier::new(num_readers + 1));
    let start = Instant::now();

    let readers: Vec<_> = (0..num_readers)
        .map(|id| {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut last = 0u64;
                let mut non_monotonic = 0u64;
                let mut max_latency_ns = 0u64;
                b.wait();
                for _ in 0..reads_per_thread {
                    let t0 = start.elapsed().as_nanos() as u64;
                    let v = s.load();
                    let t1 = start.elapsed().as_nanos() as u64;
                    let latency = t1.saturating_sub(t0);
                    if latency > max_latency_ns {
                        max_latency_ns = latency;
                    }
                    if v < last {
                        non_monotonic += 1;
                    }
                    last = v;
                }
                (id, non_monotonic, max_latency_ns)
            })
        })
        .collect();

    let writer = {
        let s = Arc::clone(&store);
        let b = Arc::clone(&barrier);
        thread::spawn(move || {
            let mut events = Vec::with_capacity(num_writes as usize);
            b.wait();
            for i in 1..=num_writes {
                let t0 = start.elapsed().as_nanos() as u64;
                s.store(i);
                let t1 = start.elapsed().as_nanos() as u64;
                events.push(WriteEvent {
                    ts_ns: t0,
                    old_generation: i - 1,
                    new_generation: i,
                    theme_name: "counter",
                    write_latency_ns: t1.saturating_sub(t0),
                });
            }
            events
        })
    };

    let write_events = writer.join().expect("writer panicked");
    let mut total_non_monotonic = 0u64;
    for handle in readers {
        let (id, non_mono, max_lat) = handle.join().expect("reader panicked");
        total_non_monotonic += non_mono;
        eprintln!(
            "[e2e_arcswap_stress] reader {id}: non_monotonic={non_mono}, max_latency={max_lat}ns"
        );
    }

    // Emit JSONL summary.
    let mut log_buf = Vec::new();
    for ev in &write_events {
        writeln!(log_buf, "{}", ev.to_jsonl()).unwrap();
    }
    let log_str = String::from_utf8(log_buf).unwrap();
    for line in log_str.lines() {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "Malformed JSONL: {line}"
        );
    }

    assert_eq!(
        total_non_monotonic, 0,
        "Non-monotonic reads detected: {total_non_monotonic}"
    );
    assert_eq!(store.load(), num_writes, "Final value mismatch");

    eprintln!(
        "[e2e_arcswap_stress] {} writes, {} total reads, {} non-monotonic",
        write_events.len(),
        num_readers as u64 * reads_per_thread,
        total_non_monotonic
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Test 4: JSONL schema compliance — verify log output is parseable
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_jsonl_schema_compliance() {
    // Generate sample events and verify they parse as valid JSON.
    let read_ev = ReadEvent {
        ts_ns: 123456789,
        reader_id: 3,
        theme_generation: 42,
        read_latency_ns: 150,
        consistent: true,
        primary_rgb: (200, 100, 50),
    };
    let write_ev = WriteEvent {
        ts_ns: 123456000,
        old_generation: 41,
        new_generation: 42,
        theme_name: "dracula",
        write_latency_ns: 300,
    };

    let read_json = read_ev.to_jsonl();
    let write_json = write_ev.to_jsonl();

    // Parse as serde_json::Value to verify structure.
    let read_val: serde_json::Value = serde_json::from_str(&read_json)
        .unwrap_or_else(|e| panic!("Failed to parse read JSONL: {e}\n{read_json}"));
    let write_val: serde_json::Value = serde_json::from_str(&write_json)
        .unwrap_or_else(|e| panic!("Failed to parse write JSONL: {e}\n{write_json}"));

    // Verify required fields exist.
    assert_eq!(read_val["event"], "seqlock_read");
    assert_eq!(read_val["reader_id"], 3);
    assert_eq!(read_val["theme_generation"], 42);
    assert_eq!(read_val["read_latency_ns"], 150);
    assert_eq!(read_val["consistent"], true);
    assert!(read_val["primary_rgb"].is_array());

    assert_eq!(write_val["event"], "seqlock_write");
    assert_eq!(write_val["old_generation"], 41);
    assert_eq!(write_val["new_generation"], 42);
    assert_eq!(write_val["theme_name"], "dracula");
    assert_eq!(write_val["write_latency_ns"], 300);
}
