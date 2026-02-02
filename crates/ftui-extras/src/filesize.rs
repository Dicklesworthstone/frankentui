#![forbid(unsafe_code)]

//! Human-friendly file size formatting utilities.
//!
//! Supports binary (KiB, MiB, GiB) and decimal (KB, MB, GB) units with
//! configurable precision and short/long unit styles.
//!
//! # Examples
//!
//! ```
//! use ftui_extras::filesize::{binary, decimal, SizeFormat};
//!
//! assert_eq!(decimal(1_500_000), "1.5 MB");
//! assert_eq!(binary(1_024), "1.0 KiB");
//!
//! let fmt = SizeFormat::decimal().with_precision(2).long();
//! assert_eq!(ftui_extras::filesize::format_size(1_536_000, fmt), "1.54 megabytes");
//! ```

/// Short unit labels for binary (1024-based) formatting.
const BINARY_UNITS_SHORT: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"];

/// Short unit labels for decimal (1000-based) formatting.
const DECIMAL_UNITS_SHORT: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];

/// Long unit labels for binary (1024-based) formatting.
const BINARY_UNITS_LONG: &[&str] = &[
    "bytes",
    "kibibytes",
    "mebibytes",
    "gibibytes",
    "tebibytes",
    "pebibytes",
    "exbibytes",
    "zebibytes",
    "yobibytes",
];

/// Long unit labels for decimal (1000-based) formatting.
const DECIMAL_UNITS_LONG: &[&str] = &[
    "bytes",
    "kilobytes",
    "megabytes",
    "gigabytes",
    "terabytes",
    "petabytes",
    "exabytes",
    "zettabytes",
    "yottabytes",
];

/// Unit system to use when formatting sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeUnit {
    /// Binary units (1024-based): KiB, MiB, GiB, etc.
    Binary,
    /// Decimal units (1000-based): KB, MB, GB, etc.
    Decimal,
}

/// Unit label style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitStyle {
    /// Short labels, e.g. "KB" / "KiB".
    Short,
    /// Long labels, e.g. "kilobytes" / "kibibytes".
    Long,
}

/// Formatting configuration for file sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeFormat {
    /// Unit system (binary or decimal).
    pub unit: SizeUnit,
    /// Unit label style (short or long).
    pub style: UnitStyle,
    /// Number of decimal places for non-byte units.
    pub precision: usize,
}

impl Default for SizeFormat {
    fn default() -> Self {
        Self::binary()
    }
}

impl SizeFormat {
    /// Binary units with short labels and 1 decimal place.
    #[must_use]
    pub const fn binary() -> Self {
        Self {
            unit: SizeUnit::Binary,
            style: UnitStyle::Short,
            precision: 1,
        }
    }

    /// Decimal units with short labels and 1 decimal place.
    #[must_use]
    pub const fn decimal() -> Self {
        Self {
            unit: SizeUnit::Decimal,
            style: UnitStyle::Short,
            precision: 1,
        }
    }

    /// Set precision (decimal places) for non-byte units.
    #[must_use]
    pub fn with_precision(mut self, precision: usize) -> Self {
        self.precision = precision;
        self
    }

    /// Use short unit labels (KB, KiB).
    #[must_use]
    pub fn short(mut self) -> Self {
        self.style = UnitStyle::Short;
        self
    }

    /// Use long unit labels (kilobytes, kibibytes).
    #[must_use]
    pub fn long(mut self) -> Self {
        self.style = UnitStyle::Long;
        self
    }
}

/// Format a size (bytes) into a human-readable string.
///
/// Bytes are always rendered without decimals (e.g., "999 B" or "999 bytes").
#[must_use]
pub fn format_size(size: i64, format: SizeFormat) -> String {
    let (base, units): (f64, &[&str]) = match (format.unit, format.style) {
        (SizeUnit::Binary, UnitStyle::Short) => (1024.0, BINARY_UNITS_SHORT),
        (SizeUnit::Binary, UnitStyle::Long) => (1024.0, BINARY_UNITS_LONG),
        (SizeUnit::Decimal, UnitStyle::Short) => (1000.0, DECIMAL_UNITS_SHORT),
        (SizeUnit::Decimal, UnitStyle::Long) => (1000.0, DECIMAL_UNITS_LONG),
    };

    let negative = size < 0;
    let abs_size = size.unsigned_abs();

    #[allow(clippy::cast_precision_loss)]
    if abs_size < base as u64 {
        let prefix = if negative { "-" } else { "" };
        return format!("{prefix}{abs_size} {}", units[0]);
    }

    #[allow(clippy::cast_precision_loss)]
    let mut value = abs_size as f64;
    let mut unit_idx = 0;
    while value >= base && unit_idx < units.len() - 1 {
        value /= base;
        unit_idx += 1;
    }

    let prefix = if negative { "-" } else { "" };
    format!(
        "{prefix}{value:.precision$} {}",
        units[unit_idx],
        precision = format.precision
    )
}

/// Format bytes using decimal (1000-based) units with 1 decimal place.
#[must_use]
pub fn decimal(size: u64) -> String {
    #[allow(clippy::cast_possible_wrap)]
    format_size(size as i64, SizeFormat::decimal())
}

/// Format bytes using decimal (1000-based) units with custom precision.
#[must_use]
pub fn decimal_with_precision(size: u64, precision: usize) -> String {
    #[allow(clippy::cast_possible_wrap)]
    format_size(size as i64, SizeFormat::decimal().with_precision(precision))
}

/// Format bytes using binary (1024-based) units with 1 decimal place.
#[must_use]
pub fn binary(size: u64) -> String {
    #[allow(clippy::cast_possible_wrap)]
    format_size(size as i64, SizeFormat::binary())
}

/// Format bytes using binary (1024-based) units with custom precision.
#[must_use]
pub fn binary_with_precision(size: u64, precision: usize) -> String {
    #[allow(clippy::cast_possible_wrap)]
    format_size(size as i64, SizeFormat::binary().with_precision(precision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_bytes_thresholds() {
        let fmt = SizeFormat::decimal();
        assert_eq!(format_size(0, fmt), "0 B");
        assert_eq!(format_size(999, fmt), "999 B");
        assert_eq!(format_size(1000, fmt), "1.0 KB");
    }

    #[test]
    fn binary_bytes_thresholds() {
        let fmt = SizeFormat::binary();
        assert_eq!(format_size(0, fmt), "0 B");
        assert_eq!(format_size(1023, fmt), "1023 B");
        assert_eq!(format_size(1024, fmt), "1.0 KiB");
    }

    #[test]
    fn decimal_precision_rounding() {
        assert_eq!(decimal_with_precision(1_234_567, 0), "1 MB");
        assert_eq!(decimal_with_precision(1_234_567, 1), "1.2 MB");
        assert_eq!(decimal_with_precision(1_234_567, 2), "1.23 MB");
        assert_eq!(decimal_with_precision(1_234_567, 3), "1.235 MB");
    }

    #[test]
    fn binary_precision_rounding() {
        assert_eq!(binary_with_precision(1_572_864, 2), "1.50 MiB");
    }

    #[test]
    fn long_unit_style() {
        let fmt = SizeFormat::decimal().long();
        assert_eq!(format_size(1_500, fmt), "1.5 kilobytes");
    }

    #[test]
    fn negative_sizes() {
        let fmt = SizeFormat::decimal();
        assert_eq!(format_size(-1_000, fmt), "-1.0 KB");
    }

    #[test]
    fn large_sizes() {
        assert_eq!(decimal(1_000_000_000_000_000), "1.0 PB");
        assert_eq!(binary(1_125_899_906_842_624), "1.0 PiB");
    }

    #[test]
    fn filesize_gb_binary() {
        assert_eq!(binary(1_073_741_824), "1.0 GiB");
        assert_eq!(binary(2_147_483_648), "2.0 GiB");
    }

    #[test]
    fn filesize_tb_binary() {
        assert_eq!(binary(1_099_511_627_776), "1.0 TiB");
    }

    #[test]
    fn filesize_gb_decimal() {
        assert_eq!(decimal(1_000_000_000), "1.0 GB");
        assert_eq!(decimal(2_500_000_000), "2.5 GB");
    }

    #[test]
    fn filesize_tb_decimal() {
        assert_eq!(decimal(1_000_000_000_000), "1.0 TB");
    }

    #[test]
    fn filesize_short_style() {
        let fmt = SizeFormat::binary().short();
        assert_eq!(format_size(1_048_576, fmt), "1.0 MiB");
    }

    #[test]
    fn filesize_long_binary_style() {
        let fmt = SizeFormat::binary().long();
        assert_eq!(format_size(1_048_576, fmt), "1.0 mebibytes");
        assert_eq!(format_size(500, fmt), "500 bytes");
    }

    #[test]
    fn filesize_edge_zero() {
        assert_eq!(decimal(0), "0 B");
        assert_eq!(binary(0), "0 B");
        let fmt = SizeFormat::decimal().long();
        assert_eq!(format_size(0, fmt), "0 bytes");
    }

    #[test]
    fn filesize_edge_one_byte() {
        assert_eq!(decimal(1), "1 B");
        assert_eq!(binary(1), "1 B");
    }

    #[test]
    fn filesize_convenience_functions() {
        assert_eq!(decimal(1_500_000), "1.5 MB");
        assert_eq!(binary(1_024), "1.0 KiB");
    }

    #[test]
    fn size_format_default_is_binary() {
        let default = SizeFormat::default();
        assert_eq!(default.unit, SizeUnit::Binary);
        assert_eq!(default.style, UnitStyle::Short);
        assert_eq!(default.precision, 1);
    }

    #[test]
    fn size_format_builder_chaining() {
        let fmt = SizeFormat::decimal().with_precision(3).long();
        assert_eq!(fmt.unit, SizeUnit::Decimal);
        assert_eq!(fmt.style, UnitStyle::Long);
        assert_eq!(fmt.precision, 3);
    }
}
