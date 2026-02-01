#![forbid(unsafe_code)]

//! Text handling for FrankenTUI.
//!
//! This crate provides text primitives for styled text rendering:
//! - [`Segment`] - atomic unit of styled text with cell-aware splitting
//! - [`SegmentLine`] - a line of segments
//! - [`SegmentLines`] - multi-line text
//! - [`Span`] - styled text span for ergonomic construction
//! - [`Line`] - a line of styled spans
//! - [`Text`] - multi-line styled text
//! - [`WidthCache`] - LRU cache for text width measurements
//!
//! # Example
//! ```
//! use ftui_text::{Segment, Text, Span, Line, WidthCache};
//! use ftui_style::Style;
//!
//! // Create styled segments (low-level)
//! let seg = Segment::styled("Error:", Style::new().bold());
//!
//! // Create styled text (high-level)
//! let text = Text::from_spans([
//!     Span::raw("Status: "),
//!     Span::styled("OK", Style::new().bold()),
//! ]);
//!
//! // Multi-line text
//! let text = Text::raw("line 1\nline 2\nline 3");
//! assert_eq!(text.height(), 3);
//!
//! // Truncate with ellipsis
//! let mut text = Text::raw("hello world");
//! text.truncate(8, Some("..."));
//! assert_eq!(text.to_plain_text(), "hello...");
//!
//! // Cache text widths for performance
//! let mut cache = WidthCache::new(1000);
//! let width = cache.get_or_compute("Hello, world!");
//! assert_eq!(width, 13);
//! ```

pub mod segment;
pub mod text;
pub mod width_cache;
pub mod wrap;

#[cfg(feature = "markup")]
pub mod markup;

/// Bounds-based text measurement for layout negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextMeasurement {
    /// Minimum possible width.
    pub minimum: usize,
    /// Maximum possible width.
    pub maximum: usize,
}

impl TextMeasurement {
    /// Zero-width measurement.
    pub const ZERO: Self = Self {
        minimum: 0,
        maximum: 0,
    };

    /// Union: take max of both bounds (side-by-side layout).
    pub fn union(self, other: Self) -> Self {
        Self {
            minimum: self.minimum.max(other.minimum),
            maximum: self.maximum.max(other.maximum),
        }
    }

    /// Stack: add both bounds (vertical stacking).
    pub fn stack(self, other: Self) -> Self {
        Self {
            minimum: self.minimum.saturating_add(other.minimum),
            maximum: self.maximum.saturating_add(other.maximum),
        }
    }

    /// Clamp bounds to optional min/max constraints.
    pub fn clamp(self, min_width: Option<usize>, max_width: Option<usize>) -> Self {
        let mut result = self;
        if let Some(min_w) = min_width {
            result.minimum = result.minimum.max(min_w);
            result.maximum = result.maximum.max(min_w);
        }
        if let Some(max_w) = max_width {
            result.minimum = result.minimum.min(max_w);
            result.maximum = result.maximum.min(max_w);
        }
        result
    }
}

pub use segment::{ControlCode, Segment, SegmentLine, SegmentLines, join_lines, split_into_lines};
pub use text::{Line, Span, Text};
pub use width_cache::{CacheStats, DEFAULT_CACHE_CAPACITY, WidthCache};
pub use wrap::{
    WrapMode, WrapOptions, ascii_width, display_width, grapheme_count, graphemes, has_wide_chars,
    is_ascii_only, truncate_to_width, truncate_to_width_with_info, truncate_with_ellipsis,
    word_boundaries, word_segments, wrap_text, wrap_with_options,
};

#[cfg(feature = "markup")]
pub use markup::{MarkupError, MarkupParser, parse_markup};

#[cfg(test)]
mod measurement_tests {
    use super::TextMeasurement;

    #[test]
    fn union_uses_max_bounds() {
        let a = TextMeasurement {
            minimum: 2,
            maximum: 8,
        };
        let b = TextMeasurement {
            minimum: 4,
            maximum: 6,
        };
        let merged = a.union(b);
        assert_eq!(
            merged,
            TextMeasurement {
                minimum: 4,
                maximum: 8
            }
        );
    }

    #[test]
    fn stack_adds_bounds() {
        let a = TextMeasurement {
            minimum: 1,
            maximum: 5,
        };
        let b = TextMeasurement {
            minimum: 2,
            maximum: 7,
        };
        let stacked = a.stack(b);
        assert_eq!(
            stacked,
            TextMeasurement {
                minimum: 3,
                maximum: 12
            }
        );
    }

    #[test]
    fn clamp_enforces_min() {
        let measurement = TextMeasurement {
            minimum: 2,
            maximum: 6,
        };
        let clamped = measurement.clamp(Some(5), None);
        assert_eq!(
            clamped,
            TextMeasurement {
                minimum: 5,
                maximum: 6
            }
        );
    }

    #[test]
    fn clamp_enforces_max() {
        let measurement = TextMeasurement {
            minimum: 4,
            maximum: 10,
        };
        let clamped = measurement.clamp(None, Some(6));
        assert_eq!(
            clamped,
            TextMeasurement {
                minimum: 4,
                maximum: 6
            }
        );
    }

    #[test]
    fn clamp_preserves_ordering() {
        let measurement = TextMeasurement {
            minimum: 3,
            maximum: 5,
        };
        let clamped = measurement.clamp(Some(7), Some(4));
        assert!(clamped.minimum <= clamped.maximum);
        assert_eq!(clamped.minimum, 4);
        assert_eq!(clamped.maximum, 4);
    }

    #[test]
    fn zero_constant() {
        assert_eq!(TextMeasurement::ZERO.minimum, 0);
        assert_eq!(TextMeasurement::ZERO.maximum, 0);
    }

    #[test]
    fn default_is_zero() {
        let m = TextMeasurement::default();
        assert_eq!(m, TextMeasurement::ZERO);
    }

    #[test]
    fn union_with_zero_is_identity() {
        let m = TextMeasurement {
            minimum: 5,
            maximum: 10,
        };
        assert_eq!(m.union(TextMeasurement::ZERO), m);
        assert_eq!(TextMeasurement::ZERO.union(m), m);
    }

    #[test]
    fn stack_with_zero_is_identity() {
        let m = TextMeasurement {
            minimum: 5,
            maximum: 10,
        };
        assert_eq!(m.stack(TextMeasurement::ZERO), m);
        assert_eq!(TextMeasurement::ZERO.stack(m), m);
    }

    #[test]
    fn stack_saturates_on_overflow() {
        let big = TextMeasurement {
            minimum: usize::MAX - 1,
            maximum: usize::MAX,
        };
        let one = TextMeasurement {
            minimum: 5,
            maximum: 5,
        };
        let stacked = big.stack(one);
        // saturating_add should prevent overflow
        assert_eq!(stacked.maximum, usize::MAX);
    }

    #[test]
    fn clamp_no_constraints() {
        let m = TextMeasurement {
            minimum: 3,
            maximum: 7,
        };
        let clamped = m.clamp(None, None);
        assert_eq!(clamped, m);
    }

    #[test]
    fn clamp_min_raises_both_bounds() {
        let m = TextMeasurement {
            minimum: 1,
            maximum: 2,
        };
        // min_width = 5 should raise both bounds
        let clamped = m.clamp(Some(5), None);
        assert_eq!(clamped.minimum, 5);
        assert_eq!(clamped.maximum, 5);
    }

    #[test]
    fn union_is_commutative() {
        let a = TextMeasurement {
            minimum: 2,
            maximum: 8,
        };
        let b = TextMeasurement {
            minimum: 4,
            maximum: 6,
        };
        assert_eq!(a.union(b), b.union(a));
    }

    #[test]
    fn stack_is_commutative() {
        let a = TextMeasurement {
            minimum: 2,
            maximum: 8,
        };
        let b = TextMeasurement {
            minimum: 4,
            maximum: 6,
        };
        assert_eq!(a.stack(b), b.stack(a));
    }
}
