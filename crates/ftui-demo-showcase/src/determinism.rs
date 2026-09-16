#![forbid(unsafe_code)]

//! Determinism fixtures for the demo showcase and tests.
//!
//! Centralizes common helpers for:
//! - deterministic mode flags
//! - seed selection
//! - stable, monotonic timestamps in logs

use std::env;
use std::sync::atomic::{AtomicU64, Ordering};

/// Parse a boolean environment flag.
///
/// Accepts "1" or "true" (case-insensitive) as enabled.
pub fn env_flag(name: &str) -> bool {
    env_flag_with(name, &env_get)
}

fn env_u64(name: &str) -> Option<u64> {
    env_u64_with(name, &env_get)
}

/// Return true if the demo is in deterministic mode.
///
/// This is a global override for screens/tests that already expose
/// per-screen deterministic flags.
pub fn is_demo_deterministic() -> bool {
    is_demo_deterministic_with(&env_get)
}

/// Resolve the demo run id (for E2E correlation).
pub fn demo_run_id() -> Option<String> {
    env::var("FTUI_DEMO_RUN_ID")
        .or_else(|_| env::var("E2E_RUN_ID"))
        .ok()
}

/// Resolve the demo screen mode label.
pub fn demo_screen_mode() -> String {
    env::var("FTUI_DEMO_SCREEN_MODE")
        .or_else(|_| env::var("FTUI_HARNESS_SCREEN_MODE"))
        .unwrap_or_else(|_| "alt".to_string())
}

/// Resolve a deterministic seed from a list of env keys.
pub fn seed_from_env(keys: &[&str], default: u64) -> u64 {
    seed_from_env_with(keys, default, &env_get)
}

/// Resolve the demo seed from standard env vars.
pub fn demo_seed(default: u64) -> u64 {
    seed_from_env(&["FTUI_DEMO_SEED", "FTUI_SEED", "E2E_SEED"], default)
}

/// Build a stable hash key from mode, size, and seed.
pub fn hash_key(mode: &str, cols: u16, rows: u16, seed: u64) -> String {
    format!("{mode}-{cols}x{rows}-seed{seed}")
}

/// Build a stable demo hash key (mode + size + seed).
pub fn demo_hash_key(cols: u16, rows: u16) -> String {
    let seed = demo_seed(0);
    let mode = demo_screen_mode();
    hash_key(&mode, cols, rows, seed)
}

/// Resolve the demo tick interval in milliseconds.
pub fn demo_tick_ms(default: u64) -> u64 {
    demo_tick_ms_override().unwrap_or(default)
}

/// Resolve an explicit demo tick override (if provided).
pub fn demo_tick_ms_override() -> Option<u64> {
    demo_tick_ms_override_with(&env_get)
}

/// Resolve a deterministic auto-exit tick count for the demo.
pub fn demo_exit_after_ticks() -> Option<u64> {
    demo_exit_after_ticks_with(&env_get)
}

fn env_get(name: &str) -> Option<String> {
    env::var(name).ok()
}

fn env_flag_with<F: Fn(&str) -> Option<String>>(name: &str, get: &F) -> bool {
    get(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn is_demo_deterministic_with<F: Fn(&str) -> Option<String>>(get: &F) -> bool {
    env_flag_with("FTUI_DEMO_DETERMINISTIC", get)
        || env_flag_with("FTUI_DETERMINISTIC", get)
        || env_flag_with("E2E_DETERMINISTIC", get)
}

fn seed_from_env_with<F: Fn(&str) -> Option<String>>(keys: &[&str], default: u64, get: &F) -> u64 {
    for key in keys {
        if let Some(raw) = get(key)
            && let Ok(value) = raw.parse::<u64>()
        {
            return value;
        }
    }
    default
}

fn env_u64_with<F: Fn(&str) -> Option<String>>(name: &str, get: &F) -> Option<u64> {
    get(name).and_then(|value| value.parse::<u64>().ok())
}

fn demo_tick_ms_override_with<F: Fn(&str) -> Option<String>>(get: &F) -> Option<u64> {
    for key in [
        "FTUI_DEMO_TICK_MS",
        "FTUI_DEMO_FIXED_TICK_MS",
        "FTUI_TICK_MS",
    ] {
        if let Some(value) = env_u64_with(key, get) {
            return Some(value);
        }
    }
    None
}

fn demo_exit_after_ticks_with<F: Fn(&str) -> Option<String>>(get: &F) -> Option<u64> {
    for key in [
        "FTUI_DEMO_EXIT_AFTER_TICKS",
        "FTUI_DEMO_EXIT_TICKS",
        "FTUI_DEMO_EXIT_AFTER_FRAMES",
    ] {
        if let Some(value) = env_u64_with(key, get) {
            return Some(value);
        }
    }
    None
}

/// ISO-8601-like monotonic timestamp without external deps.
///
/// Intended for deterministic JSONL logs in tests.
pub fn chrono_like_timestamp() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("T{n:06}")
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Capture a stable environment snapshot for demo determinism logs.
pub fn demo_env_json() -> String {
    let deterministic = is_demo_deterministic();
    let seed = demo_seed(0);
    let screen_mode = demo_screen_mode();
    let ui_height = env_u64("FTUI_DEMO_UI_HEIGHT");
    let ui_min_height = env_u64("FTUI_DEMO_UI_MIN_HEIGHT");
    let ui_max_height = env_u64("FTUI_DEMO_UI_MAX_HEIGHT");
    let tick_ms = demo_tick_ms(100);
    let exit_after_ticks = demo_exit_after_ticks();
    let run_id = demo_run_id();
    let term = env::var("TERM").unwrap_or_default();
    let colorterm = env::var("COLORTERM").unwrap_or_default();
    let no_color = env::var("NO_COLOR").is_ok();

    let run_id_json = run_id
        .as_ref()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_string());
    let ui_height_json = ui_height
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let ui_min_height_json = ui_min_height
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let ui_max_height_json = ui_max_height
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let exit_after_ticks_json = exit_after_ticks
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());

    format!(
        "{{\"deterministic\":{},\"seed\":{},\"screen_mode\":\"{}\",\"ui_height\":{},\"ui_min_height\":{},\"ui_max_height\":{},\"tick_ms\":{},\"exit_after_ticks\":{},\"run_id\":{},\"term\":\"{}\",\"colorterm\":\"{}\",\"no_color\":{}}}",
        deterministic,
        seed,
        json_escape(&screen_mode),
        ui_height_json,
        ui_min_height_json,
        ui_max_height_json,
        tick_ms,
        exit_after_ticks_json,
        run_id_json,
        json_escape(&term),
        json_escape(&colorterm),
        no_color
    )
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn map_getter<'a>(map: &'a BTreeMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| map.get(key).map(|value| value.to_string())
    }

    #[test]
    fn demo_deterministic_flag_respects_env() {
        let empty = BTreeMap::new();
        let get = map_getter(&empty);
        assert!(
            !is_demo_deterministic_with(&get),
            "expected demo deterministic mode to be disabled when no flags are set"
        );

        let mut demo = BTreeMap::new();
        demo.insert("FTUI_DEMO_DETERMINISTIC", "1");
        let get = map_getter(&demo);
        assert!(
            is_demo_deterministic_with(&get),
            "expected FTUI_DEMO_DETERMINISTIC to enable deterministic mode"
        );

        let mut global = BTreeMap::new();
        global.insert("FTUI_DETERMINISTIC", "true");
        let get = map_getter(&global);
        assert!(
            is_demo_deterministic_with(&get),
            "expected FTUI_DETERMINISTIC to enable deterministic mode"
        );

        let mut e2e = BTreeMap::new();
        e2e.insert("E2E_DETERMINISTIC", "1");
        let get = map_getter(&e2e);
        assert!(
            is_demo_deterministic_with(&get),
            "expected E2E_DETERMINISTIC to enable deterministic mode"
        );
    }

    #[test]
    fn demo_seed_default_when_unset() {
        assert_eq!(
            seed_from_env_with(&["FTUI_DEMO_SEED", "FTUI_SEED", "E2E_SEED"], 7, &|_| None),
            7
        );
    }

    #[test]
    fn demo_seed_respects_env_priority() {
        let empty = BTreeMap::new();
        let get = map_getter(&empty);
        assert_eq!(
            seed_from_env_with(&["FTUI_DEMO_SEED", "FTUI_SEED", "E2E_SEED"], 7, &get),
            7,
            "expected default seed when no env vars are set"
        );

        let mut e2e = BTreeMap::new();
        e2e.insert("E2E_SEED", "9");
        let get = map_getter(&e2e);
        assert_eq!(
            seed_from_env_with(&["FTUI_DEMO_SEED", "FTUI_SEED", "E2E_SEED"], 7, &get),
            9,
            "expected E2E_SEED to be used when others are unset"
        );

        let mut ftui = BTreeMap::new();
        ftui.insert("FTUI_SEED", "11");
        let get = map_getter(&ftui);
        assert_eq!(
            seed_from_env_with(&["FTUI_DEMO_SEED", "FTUI_SEED", "E2E_SEED"], 7, &get),
            11,
            "expected FTUI_SEED to override E2E_SEED"
        );

        let mut demo = BTreeMap::new();
        demo.insert("FTUI_DEMO_SEED", "13");
        demo.insert("FTUI_SEED", "11");
        demo.insert("E2E_SEED", "9");
        let get = map_getter(&demo);
        assert_eq!(
            seed_from_env_with(&["FTUI_DEMO_SEED", "FTUI_SEED", "E2E_SEED"], 7, &get),
            13,
            "expected FTUI_DEMO_SEED to have highest priority"
        );
    }

    #[test]
    fn timestamp_is_monotonic() {
        let first = chrono_like_timestamp();
        let second = chrono_like_timestamp();
        assert!(
            second > first,
            "expected chrono_like_timestamp to be monotonic: {first} -> {second}"
        );
    }

    #[test]
    fn demo_tick_override_respects_env_priority() {
        let empty = BTreeMap::new();
        let get = map_getter(&empty);
        assert!(
            demo_tick_ms_override_with(&get).is_none(),
            "expected no override when tick env vars are unset"
        );

        let mut global = BTreeMap::new();
        global.insert("FTUI_TICK_MS", "120");
        let get = map_getter(&global);
        assert_eq!(
            demo_tick_ms_override_with(&get),
            Some(120),
            "expected FTUI_TICK_MS to be used when demo overrides are unset"
        );

        let mut fixed = BTreeMap::new();
        fixed.insert("FTUI_TICK_MS", "120");
        fixed.insert("FTUI_DEMO_FIXED_TICK_MS", "90");
        let get = map_getter(&fixed);
        assert_eq!(
            demo_tick_ms_override_with(&get),
            Some(90),
            "expected FTUI_DEMO_FIXED_TICK_MS to override FTUI_TICK_MS"
        );

        let mut demo = BTreeMap::new();
        demo.insert("FTUI_TICK_MS", "120");
        demo.insert("FTUI_DEMO_FIXED_TICK_MS", "90");
        demo.insert("FTUI_DEMO_TICK_MS", "60");
        let get = map_getter(&demo);
        assert_eq!(
            demo_tick_ms_override_with(&get),
            Some(60),
            "expected FTUI_DEMO_TICK_MS to have highest priority"
        );
    }

    #[test]
    fn demo_exit_after_ticks_respects_env_priority() {
        let empty = BTreeMap::new();
        let get = map_getter(&empty);
        assert!(
            demo_exit_after_ticks_with(&get).is_none(),
            "expected no exit-after-ticks when env vars are unset"
        );

        let mut frames = BTreeMap::new();
        frames.insert("FTUI_DEMO_EXIT_AFTER_FRAMES", "33");
        let get = map_getter(&frames);
        assert_eq!(
            demo_exit_after_ticks_with(&get),
            Some(33),
            "expected FTUI_DEMO_EXIT_AFTER_FRAMES to be used when others are unset"
        );

        let mut alias = BTreeMap::new();
        alias.insert("FTUI_DEMO_EXIT_AFTER_FRAMES", "33");
        alias.insert("FTUI_DEMO_EXIT_TICKS", "44");
        let get = map_getter(&alias);
        assert_eq!(
            demo_exit_after_ticks_with(&get),
            Some(44),
            "expected FTUI_DEMO_EXIT_TICKS to override EXIT_AFTER_FRAMES"
        );

        let mut primary = BTreeMap::new();
        primary.insert("FTUI_DEMO_EXIT_AFTER_FRAMES", "33");
        primary.insert("FTUI_DEMO_EXIT_TICKS", "44");
        primary.insert("FTUI_DEMO_EXIT_AFTER_TICKS", "55");
        let get = map_getter(&primary);
        assert_eq!(
            demo_exit_after_ticks_with(&get),
            Some(55),
            "expected FTUI_DEMO_EXIT_AFTER_TICKS to have highest priority"
        );
    }

    #[test]
    fn hash_key_matches_e2e_format() {
        assert_eq!(hash_key("inline", 80, 24, 42), "inline-80x24-seed42");
    }
}
