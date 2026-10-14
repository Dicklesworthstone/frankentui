#![forbid(unsafe_code)]

//! Zero-cost debug tracing controlled by environment variable.
//!
//! Enable runtime debug output by setting `FTUI_DEBUG_TRACE=1` before launching
//! your application, with stderr redirected. When disabled (the default), the
//! trace checks compile down to a single static bool load with no other
//! overhead.
//!
//! The trace stays off while stderr is a terminal: that is the terminal the UI
//! draws on, and a trace line written there lands between frames
//! (bd-g00-root-epic-ewths.33.4).
//!
//! # Usage
//!
//! ```bash
//! FTUI_DEBUG_TRACE=1 cargo run -p ftui-demo-showcase 2>trace.log
//! ```
//!
//! This module provides `debug_trace!` macro for conditional debug output:
//!
//! ```ignore
//! use ftui_runtime::debug_trace;
//! debug_trace!("loop iteration {}", count);
//! ```

use std::sync::LazyLock;
use web_time::Instant;

/// Static flag checked once at startup. After initialization, this is just a bool load.
static DEBUG_TRACE_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    trace_enabled(
        std::env::var("FTUI_DEBUG_TRACE").ok().as_deref(),
        std::io::IsTerminal::is_terminal(&std::io::stderr()),
    )
});

/// `FTUI_DEBUG_TRACE` is `1` or `true`, and stderr is not a terminal.
fn trace_enabled(flag: Option<&str>, stderr_is_terminal: bool) -> bool {
    let requested = flag.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    requested && !stderr_is_terminal
}

/// Startup timestamp for relative timing in debug output.
static START_TIME: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Check if debug tracing is enabled.
///
/// This function is inlined and after the first call, compiles down to a single
/// static bool load - effectively zero cost when disabled.
#[inline]
pub fn is_enabled() -> bool {
    *DEBUG_TRACE_ENABLED
}

/// Get elapsed time since program start in milliseconds.
///
/// Useful for correlating debug output across threads.
#[inline]
pub fn elapsed_ms() -> u64 {
    START_TIME.elapsed().as_millis() as u64
}

/// Conditionally print debug trace output to stderr.
///
/// When `FTUI_DEBUG_TRACE=1` is set and stderr is not a terminal, prints
/// timestamped debug messages. When disabled, compiles to a single bool check
/// (effectively zero cost).
///
/// # Example
///
/// ```ignore
/// debug_trace!("subscription started: id={}", sub_id);
/// debug_trace!("main loop heartbeat: frame={}", frame_count);
/// ```
#[macro_export]
macro_rules! debug_trace {
    ($($arg:tt)*) => {
        if $crate::debug_trace::is_enabled() {
            eprintln!(
                "[FTUI {:>8}ms] {}",
                $crate::debug_trace::elapsed_ms(),
                format_args!($($arg)*)
            );
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_needs_the_flag_and_a_stderr_that_is_not_the_terminal() {
        assert!(trace_enabled(Some("1"), false));
        assert!(trace_enabled(Some("TRUE"), false));
        assert!(
            !trace_enabled(Some("1"), true),
            "stderr is the UI's terminal"
        );
        assert!(!trace_enabled(Some("0"), false));
        assert!(!trace_enabled(None, false));
    }

    #[test]
    fn test_is_enabled_returns_bool() {
        // Just verify it returns a bool without panicking
        let _ = is_enabled();
    }

    #[test]
    fn test_elapsed_ms_increases() {
        let t1 = elapsed_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = elapsed_ms();
        assert!(t2 >= t1);
    }
}
