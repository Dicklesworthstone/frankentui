#![forbid(unsafe_code)]

//! Error types for locale-aware formatting.

use std::fmt;

/// Errors that can occur during locale-aware formatting.
#[derive(Debug, Clone, PartialEq)]
pub enum FormattingError {
    /// The requested locale is not supported and no suitable fallback was found.
    UnsupportedLocale(String),
    /// A non-finite floating point value (NaN or infinity) was encountered.
    NonFinite(f64),
    /// A date or time component was invalid.
    InvalidDateTime(DateTimeError),
    /// An invalid formatting pattern or configuration was provided.
    InvalidFormat(String),
    /// A numeric value is out of range for the requested format.
    OutOfRange(String),
}

impl fmt::Display for FormattingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedLocale(loc) => write!(f, "unsupported locale: '{loc}'"),
            Self::NonFinite(v) => write!(f, "cannot format non-finite float: {v}"),
            Self::InvalidDateTime(e) => write!(f, "invalid date/time: {e}"),
            Self::InvalidFormat(msg) => write!(f, "invalid format: {msg}"),
            Self::OutOfRange(msg) => write!(f, "value out of range: {msg}"),
        }
    }
}

impl std::error::Error for FormattingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidDateTime(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DateTimeError> for FormattingError {
    fn from(err: DateTimeError) -> Self {
        Self::InvalidDateTime(err)
    }
}

/// Errors in date or time construction and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateTimeError {
    /// Month is not in the range 1..=12.
    InvalidMonth(u8),
    /// Day is not valid for the given month and year.
    InvalidDay {
        year: i32,
        month: u8,
        day: u8,
        max_days: u8,
    },
    /// Hour is not in the range 0..=23.
    InvalidHour(u8),
    /// Minute is not in the range 0..=59.
    InvalidMinute(u8),
    /// Second is not in the range 0..=59.
    InvalidSecond(u8),
    /// Millisecond is not in the range 0..=999.
    InvalidMillisecond(u16),
    /// Timezone offset in minutes is not in the range -720..=840 (-12h..+14h).
    InvalidTimezoneOffset(i16),
}

impl fmt::Display for DateTimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMonth(m) => write!(f, "month {m} out of range (1..=12)"),
            Self::InvalidDay {
                year,
                month,
                day,
                max_days,
            } => write!(
                f,
                "day {day} out of range for {year:04}-{month:02} (max {max_days})"
            ),
            Self::InvalidHour(h) => write!(f, "hour {h} out of range (0..=23)"),
            Self::InvalidMinute(m) => write!(f, "minute {m} out of range (0..=59)"),
            Self::InvalidSecond(s) => write!(f, "second {s} out of range (0..=59)"),
            Self::InvalidMillisecond(ms) => {
                write!(f, "millisecond {ms} out of range (0..=999)")
            }
            Self::InvalidTimezoneOffset(offset) => {
                write!(f, "timezone offset {offset}m out of range (-720..=840)")
            }
        }
    }
}

impl std::error::Error for DateTimeError {}
