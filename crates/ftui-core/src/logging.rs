#![forbid(unsafe_code)]

//! Logging and tracing support.
//!
//! This module provides re-exports of tracing macros when the `tracing` feature is enabled.
//! When the feature is disabled, no-op macros are provided for compatibility.

#[cfg(feature = "tracing")]
pub use tracing::{
    debug, debug_span, error, error_span, info, info_span, trace, trace_span, warn, warn_span,
};

// When tracing is not enabled, provide no-op macros
#[cfg(not(feature = "tracing"))]
mod noop_macros {
    /// No-op debug macro when tracing is disabled.
    #[macro_export]
    macro_rules! debug {
        ($($arg:tt)*) => {};
    }

    /// No-op debug_span macro when tracing is disabled.
    #[macro_export]
    macro_rules! debug_span {
        ($($arg:tt)*) => {
            $crate::logging::NoopSpan
        };
    }

    /// No-op error macro when tracing is disabled.
    #[macro_export]
    macro_rules! error {
        ($($arg:tt)*) => {};
    }

    /// No-op error_span macro when tracing is disabled.
    #[macro_export]
    macro_rules! error_span {
        ($($arg:tt)*) => {
            $crate::logging::NoopSpan
        };
    }

    /// No-op info macro when tracing is disabled.
    #[macro_export]
    macro_rules! info {
        ($($arg:tt)*) => {};
    }

    /// No-op info_span macro when tracing is disabled.
    #[macro_export]
    macro_rules! info_span {
        ($($arg:tt)*) => {
            $crate::logging::NoopSpan
        };
    }

    /// No-op trace macro when tracing is disabled.
    #[macro_export]
    macro_rules! trace {
        ($($arg:tt)*) => {};
    }

    /// No-op trace_span macro when tracing is disabled.
    #[macro_export]
    macro_rules! trace_span {
        ($($arg:tt)*) => {
            $crate::logging::NoopSpan
        };
    }

    /// No-op warn macro when tracing is disabled.
    #[macro_export]
    macro_rules! warn {
        ($($arg:tt)*) => {};
    }

    /// No-op warn_span macro when tracing is disabled.
    #[macro_export]
    macro_rules! warn_span {
        ($($arg:tt)*) => {
            $crate::logging::NoopSpan
        };
    }
}

// Note: Macros are exported at crate root via #[macro_export],
// so we don't need to re-export noop_macros::* here.

/// A no-op span guard for when tracing is disabled.
#[cfg(not(feature = "tracing"))]
pub struct NoopSpan;

#[cfg(not(feature = "tracing"))]
impl NoopSpan {
    /// Enter the no-op span (does nothing).
    pub fn enter(&self) -> NoopGuard {
        NoopGuard
    }
}

/// A no-op span guard.
#[cfg(not(feature = "tracing"))]
pub struct NoopGuard;

#[cfg(test)]
#[cfg(not(feature = "tracing"))]
mod tests {
    use super::*;
    // Import #[macro_export] macros from crate root
    use crate::{
        debug, debug_span, error, error_span, info, info_span, trace, trace_span, warn, warn_span,
    };

    #[test]
    fn noop_span_enter_returns_guard() {
        let span = NoopSpan;
        let _guard = span.enter();
    }

    #[test]
    fn noop_span_enter_multiple_times() {
        let span = NoopSpan;
        let _g1 = span.enter();
        let _g2 = span.enter();
        let _g3 = span.enter();
    }

    #[test]
    fn noop_guard_drops_silently() {
        let span = NoopSpan;
        {
            let _guard = span.enter();
        }
        // Guard dropped without issue
    }

    #[test]
    fn noop_macros_compile_and_are_silent() {
        // These should all compile to nothing
        debug!("test message");
        info!("test {}", 42);
        warn!("warning {val}", val = "test");
        error!("error");
        trace!("trace");
    }

    #[test]
    fn noop_span_macros_return_noop_span() {
        let span = debug_span!("test_span");
        let _guard = span.enter();

        let span2 = info_span!("other_span", key = "value");
        let _guard2 = span2.enter();
    }

    #[test]
    fn noop_span_macros_all_levels() {
        let _s1 = debug_span!("d");
        let _s2 = error_span!("e");
        let _s3 = info_span!("i");
        let _s4 = trace_span!("t");
        let _s5 = warn_span!("w");
    }

    #[test]
    fn entered_pattern() {
        // Common pattern: let _span = span!(...).entered()
        // NoopSpan doesn't have .entered(), it uses .enter()
        // Verify the pattern works
        let _guard = debug_span!("my_span").enter();
    }
}
