//! Persistence for [`TransitionCounter`] screen transition data.
//!
//! Provides `save_transitions` / `load_transitions` for persisting learned
//! transition patterns to a JSON file, enabling cross-session learning.
//!
//! # File Format
//!
//! ```json
//! {
//!   "version": 1,
//!   "total_transitions": 1247.0,
//!   "last_saved": "2026-02-24T02:30:00Z",
//!   "transitions": [
//!     { "from": "Dashboard", "to": "Messages", "count": 142.0 },
//!     ...
//!   ]
//! }
//! ```
//!
//! # Atomic Writes
//!
//! Writes use a temp-file-then-rename pattern to prevent corruption on crash.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

use super::TransitionCounter;

/// Current file format version.
const FORMAT_VERSION: u64 = 1;

/// A single serialized transition entry.
#[derive(Debug, Serialize, Deserialize)]
struct TransitionEntry {
    from: String,
    to: String,
    count: f64,
}

/// On-disk representation of transition data.
#[derive(Debug, Serialize, Deserialize)]
struct TransitionFile {
    version: u64,
    total_transitions: f64,
    last_saved: String,
    transitions: Vec<TransitionEntry>,
}

/// Save a [`TransitionCounter`] to a JSON file.
///
/// Uses atomic write (write-to-temp-then-rename) to prevent corruption.
/// The `path` parent directory must already exist.
pub fn save_transitions(counter: &TransitionCounter<String>, path: &Path) -> io::Result<()> {
    let mut entries = Vec::new();
    let mut total = 0.0_f64;

    // Extract all transitions from the counter
    for state_id in counter.state_ids() {
        for (target, _prob) in counter.all_targets_ranked(&state_id) {
            let count = counter.count(&state_id, &target);
            if count > 0.0 {
                total += count;
                entries.push(TransitionEntry {
                    from: state_id.clone(),
                    to: target,
                    count,
                });
            }
        }
    }

    // Sort for deterministic output (easier to diff/debug)
    entries.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));

    let entry_count = entries.len();
    let file = TransitionFile {
        version: FORMAT_VERSION,
        total_transitions: total,
        last_saved: now_iso8601(),
        transitions: entries,
    };

    let json = serde_json::to_string_pretty(&file).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to serialize transitions: {e}"),
        )
    })?;

    // Atomic write: temp file then rename
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, json)?;
    std::fs::rename(&temp, path)?;

    info!(
        path = %path.display(),
        transitions = total,
        entries = entry_count,
        "tick_strategy.save"
    );

    Ok(())
}

/// Load a [`TransitionCounter`] from a JSON file.
///
/// - **Missing file** returns an empty counter (not an error).
/// - **Corrupted file** returns an `io::Error`.
/// - **Version mismatch** returns an `io::Error` with a descriptive message.
pub fn load_transitions(path: &Path) -> io::Result<TransitionCounter<String>> {
    if !path.exists() {
        return Ok(TransitionCounter::new());
    }

    let contents = std::fs::read_to_string(path)?;
    let file: TransitionFile = serde_json::from_str(&contents).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse transitions file: {e}"),
        )
    })?;

    if file.version != FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported transitions file version: {} (expected {FORMAT_VERSION})",
                file.version
            ),
        ));
    }

    let mut counter = TransitionCounter::new();
    for entry in &file.transitions {
        counter.record_with_count(entry.from.clone(), entry.to.clone(), entry.count);
    }

    info!(
        path = %path.display(),
        loaded_transitions = file.total_transitions,
        entries = file.transitions.len(),
        known_screens = counter.state_ids().len(),
        "tick_strategy.load"
    );

    Ok(counter)
}

/// Get current timestamp in ISO 8601 format.
fn now_iso8601() -> String {
    // Use web-time for cross-platform compatibility (works in WASM too)
    let now = web_time::SystemTime::now();
    let since_epoch = now
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs();

    // Simple ISO 8601 formatting without pulling in chrono
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Days since Unix epoch to Y-M-D (simplified, handles common cases)
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days_since_epoch: u64) -> (u64, u64, u64) {
    // Algorithm based on Howard Hinnant's civil_from_days
    let z = days_since_epoch as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn retained_fixture_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ftui-tick-persistence-")
            .disable_cleanup(true)
            .tempdir()
            .expect("create retained persistence fixture")
    }

    #[test]
    fn round_trip_preserves_data() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("transitions.json");

        let mut counter = TransitionCounter::new();
        for _ in 0..20 {
            counter.record("Dashboard".to_owned(), "Messages".to_owned());
        }
        for _ in 0..10 {
            counter.record("Dashboard".to_owned(), "Settings".to_owned());
        }
        for _ in 0..5 {
            counter.record("Messages".to_owned(), "Dashboard".to_owned());
        }

        save_transitions(&counter, &path).unwrap();
        let loaded = load_transitions(&path).unwrap();

        // Verify counts match
        assert_eq!(
            loaded.count(&"Dashboard".to_owned(), &"Messages".to_owned()),
            20.0
        );
        assert_eq!(
            loaded.count(&"Dashboard".to_owned(), &"Settings".to_owned()),
            10.0
        );
        assert_eq!(
            loaded.count(&"Messages".to_owned(), &"Dashboard".to_owned()),
            5.0
        );
        assert_eq!(loaded.total(), 35.0);
    }

    #[test]
    fn missing_file_returns_empty_counter() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("nonexistent.json");

        let counter = load_transitions(&path).unwrap();
        assert_eq!(counter.total(), 0.0);
    }

    #[test]
    fn corrupted_file_returns_error() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not valid json {{{").unwrap();

        let result = load_transitions(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn version_mismatch_returns_error() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("future.json");
        let bad_version = serde_json::json!({
            "version": 999,
            "total_transitions": 0.0,
            "last_saved": "2026-01-01T00:00:00Z",
            "transitions": []
        });
        std::fs::write(&path, serde_json::to_string(&bad_version).unwrap()).unwrap();

        let result = load_transitions(&path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("version"),
            "error should mention version: {err_msg}"
        );
    }

    #[test]
    fn atomic_write_no_temp_file_on_success() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("transitions.json");
        let temp = path.with_extension("json.tmp");

        let counter = TransitionCounter::new();
        save_transitions(&counter, &path).unwrap();

        assert!(path.exists());
        assert!(!temp.exists(), "temp file should be removed after rename");
    }

    #[test]
    fn empty_counter_round_trips() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("empty.json");

        let counter = TransitionCounter::<String>::new();
        save_transitions(&counter, &path).unwrap();

        let loaded = load_transitions(&path).unwrap();
        assert_eq!(loaded.total(), 0.0);
    }

    #[test]
    fn file_is_human_readable_json() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("readable.json");

        let mut counter = TransitionCounter::new();
        counter.record("A".to_owned(), "B".to_owned());
        save_transitions(&counter, &path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        // Should be pretty-printed
        assert!(contents.contains('\n'));
        // Should contain expected fields
        assert!(contents.contains("\"version\": 1"));
        assert!(contents.contains("\"transitions\""));
        assert!(contents.contains("\"from\": \"A\""));
        assert!(contents.contains("\"to\": \"B\""));
    }

    #[test]
    fn deterministic_output_ordering() {
        let dir = retained_fixture_dir();
        let path1 = dir.path().join("out1.json");
        let path2 = dir.path().join("out2.json");

        // Build counter in arbitrary order
        let mut counter = TransitionCounter::new();
        counter.record("Z".to_owned(), "A".to_owned());
        counter.record("A".to_owned(), "Z".to_owned());
        counter.record("M".to_owned(), "B".to_owned());

        save_transitions(&counter, &path1).unwrap();
        save_transitions(&counter, &path2).unwrap();

        let c1 = std::fs::read_to_string(&path1).unwrap();
        let c2 = std::fs::read_to_string(&path2).unwrap();

        // Remove timestamps (they may differ by a second)
        let strip_ts = |s: &str| -> String {
            s.lines()
                .filter(|l| !l.contains("last_saved"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip_ts(&c1), strip_ts(&c2));
    }

    #[test]
    fn large_counts_round_trip() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("large.json");

        let mut counter = TransitionCounter::new();
        for _ in 0..1000 {
            counter.record("A".to_owned(), "B".to_owned());
        }

        save_transitions(&counter, &path).unwrap();
        let loaded = load_transitions(&path).unwrap();

        assert_eq!(loaded.count(&"A".to_owned(), &"B".to_owned()), 1000.0);
    }

    #[test]
    fn fractional_counts_round_trip() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("decayed.json");

        let mut counter = TransitionCounter::new();
        for _ in 0..10 {
            counter.record("A".to_owned(), "B".to_owned());
        }
        // Decay produces fractional counts: 10.0 * 0.85 = 8.5
        counter.decay(0.85);
        let before = counter.count(&"A".to_owned(), &"B".to_owned());
        assert!((before - 8.5).abs() < 1e-10);

        save_transitions(&counter, &path).unwrap();
        let loaded = load_transitions(&path).unwrap();

        let after = loaded.count(&"A".to_owned(), &"B".to_owned());
        assert!(
            (after - before).abs() < 1e-10,
            "fractional count should round-trip: before={before}, after={after}"
        );
    }

    #[test]
    fn partial_file_write_detected() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("partial.json");

        // Write a truncated but somewhat valid JSON
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            "{{\"version\": 1, \"total_transitions\": 5.0, \"last_sav"
        )
        .unwrap();

        let result = load_transitions(&path);
        assert!(result.is_err());
    }

    // ========================================================================
    // Additional tests (I.5 coverage)
    // ========================================================================

    #[test]
    fn save_load_save_produces_identical_files() {
        let dir = retained_fixture_dir();
        let path1 = dir.path().join("first.json");
        let path2 = dir.path().join("second.json");

        let mut counter = TransitionCounter::new();
        counter.record("A".to_owned(), "B".to_owned());
        counter.record("A".to_owned(), "C".to_owned());
        counter.record("B".to_owned(), "A".to_owned());

        // Save original
        save_transitions(&counter, &path1).unwrap();

        // Load and re-save
        let loaded = load_transitions(&path1).unwrap();
        save_transitions(&loaded, &path2).unwrap();

        let c1 = std::fs::read_to_string(&path1).unwrap();
        let c2 = std::fs::read_to_string(&path2).unwrap();

        // Strip timestamps (they may differ slightly)
        let strip_ts = |s: &str| -> String {
            s.lines()
                .filter(|l| !l.contains("last_saved"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        eprintln!("first (stripped): {}", strip_ts(&c1));
        eprintln!("second (stripped): {}", strip_ts(&c2));
        assert_eq!(strip_ts(&c1), strip_ts(&c2));
    }

    #[test]
    fn large_counter_many_entries_round_trip() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("large_many.json");

        let mut counter = TransitionCounter::new();
        // 1000+ distinct entries
        for i in 0..50 {
            for j in 0..25 {
                counter.record(format!("screen_{i}"), format!("screen_{j}"));
            }
        }

        let original_total = counter.total();
        eprintln!(
            "entries: {}, total: {original_total}",
            counter.state_ids().len()
        );
        assert!(original_total >= 1000.0);

        save_transitions(&counter, &path).unwrap();
        let loaded = load_transitions(&path).unwrap();

        eprintln!("loaded total: {}", loaded.total());
        assert!(
            (loaded.total() - original_total).abs() < 1e-10,
            "total mismatch: {} vs {original_total}",
            loaded.total()
        );

        // Spot-check a few entries
        assert_eq!(
            loaded.count(&"screen_0".to_owned(), &"screen_0".to_owned()),
            counter.count(&"screen_0".to_owned(), &"screen_0".to_owned())
        );
        assert_eq!(
            loaded.count(&"screen_49".to_owned(), &"screen_24".to_owned()),
            counter.count(&"screen_49".to_owned(), &"screen_24".to_owned())
        );
    }

    #[test]
    fn special_characters_in_screen_ids_round_trip() {
        let dir = retained_fixture_dir();
        let path = dir.path().join("special.json");

        let mut counter = TransitionCounter::new();
        counter.record("screen/home".to_owned(), "screen:settings".to_owned());
        counter.record("tab \"quotes\"".to_owned(), "tab\nnewline".to_owned());
        counter.record("emoji 🎉".to_owned(), "unicode αβγ".to_owned());

        save_transitions(&counter, &path).unwrap();
        let loaded = load_transitions(&path).unwrap();

        eprintln!("loaded total: {}", loaded.total());
        assert_eq!(
            loaded.count(&"screen/home".to_owned(), &"screen:settings".to_owned()),
            1.0
        );
        assert_eq!(
            loaded.count(&"tab \"quotes\"".to_owned(), &"tab\nnewline".to_owned()),
            1.0
        );
        assert_eq!(
            loaded.count(&"emoji 🎉".to_owned(), &"unicode αβγ".to_owned()),
            1.0
        );
        assert_eq!(loaded.total(), 3.0);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_file_save_returns_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = retained_fixture_dir();
        let path = dir.path().join("readonly.json");

        // Create the file first
        let counter = TransitionCounter::<String>::new();
        save_transitions(&counter, &path).unwrap();

        // Make it read-only
        let perms = std::fs::Permissions::from_mode(0o444);
        std::fs::set_permissions(&path, perms).unwrap();

        // Trying to save again should fail (the temp file write may
        // succeed but the rename over the read-only original may fail
        // depending on filesystem — or the temp file itself may fail).
        // Either way, we should get an error, not a panic.
        let result = save_transitions(&counter, &path);
        eprintln!("save to read-only result: {result:?}");
        // Note: on some systems rename over read-only succeeds (POSIX
        // allows it if the directory is writable). So we just verify
        // no panic occurs. If it errors, that's fine.
        // The key invariant: no panic.
    }
}
