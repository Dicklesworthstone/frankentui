#![forbid(unsafe_code)]

//! Locale-aware number, currency, percent, and date/time formatting.
//!
//! Provides deterministic formatting backed by pinned Unicode CLDR v45.0 data
//! for all 7 supported FrankenTUI demo languages: English, Spanish, French,
//! German, Russian, Arabic, and Japanese.

pub mod data;
pub mod datetime;
pub mod error;
pub mod number;

pub use data::{
    lookup_locale_data, CurrencyPlacement, DateSymbols, DateTimeOrder, LocaleData, NumberSymbols,
    PercentPlacement, CLDR_VERSION, SUPPORTED_LOCALES,
};
pub use datetime::{
    days_in_month, is_leap_year, Date, DateFormatStyle, DateTime, DateTimeFormatter, Time,
    TimeFormatStyle,
};
pub use error::{DateTimeError, FormattingError};
pub use number::{
    NumberFormat, NumberFormatter, NumberStyle, NumberingSystem, RoundingMode,
};
