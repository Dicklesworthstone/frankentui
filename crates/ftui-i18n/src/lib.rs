#![forbid(unsafe_code)]

//! Internationalization (i18n) foundation for FrankenTUI.
//!
//! Provides externalized string storage with key-based lookup,
//! locale fallback chains, ICU-style plural forms, and variable
//! interpolation.
//!
//! # Role in FrankenTUI
//! `ftui-i18n` isolates localization concerns so widgets and apps can
//! remain deterministic while still supporting multiple languages.
//!
//! # How it fits in the system
//! Widgets and demo screens can depend on this crate to resolve strings
//! into localized text before rendering. It does not depend on rendering or
//! runtime, keeping the localization layer reusable and testable.

pub mod catalog;
pub mod format;
pub mod plural;

pub use catalog::{
    CoverageReport, I18nError, LocaleCoverage, LocaleStrings, StringCatalog, StringEntry,
};
pub use format::{
    days_in_month, is_leap_year, lookup_locale_data, CurrencyPlacement, Date, DateFormatStyle,
    DateSymbols, DateTime, DateTimeError, DateTimeFormatter, DateTimeOrder, FormattingError,
    LocaleData, NumberFormat, NumberFormatter, NumberStyle, NumberSymbols, NumberingSystem,
    PercentPlacement, RoundingMode, Time, TimeFormatStyle, CLDR_VERSION, SUPPORTED_LOCALES,
};
pub use plural::{PluralCategory, PluralForms, PluralRule};
