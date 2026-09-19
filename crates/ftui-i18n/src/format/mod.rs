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
    CLDR_VERSION, CurrencyPlacement, DateSymbols, DateTimeOrder, LocaleData, NumberSymbols,
    PercentPlacement, SUPPORTED_LOCALES, lookup_locale_data,
};
pub use datetime::{
    Date, DateFormatStyle, DateTime, DateTimeFormatter, Time, TimeFormatStyle, days_in_month,
    is_leap_year,
};
pub use error::{DateTimeError, FormattingError};
pub use number::{NumberFormat, NumberFormatter, NumberStyle, NumberingSystem, RoundingMode};
