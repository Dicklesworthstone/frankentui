#![forbid(unsafe_code)]

//! Locale-aware date and time representation and formatting.

use super::data::{DateSymbols, DateTimeOrder, LocaleData, lookup_locale_data};
use super::error::{DateTimeError, FormattingError};

/// Date formatting style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateFormatStyle {
    /// Numeric short format (e.g. `09/19/2026`, `19.09.2026`).
    #[default]
    Short,
    /// Abbreviated month format (e.g. `Sep 19, 2026`, `19. Sep. 2026`).
    Medium,
    /// Full month name format (e.g. `September 19, 2026`, `19. September 2026`).
    Long,
    /// Full day-of-week and month format (e.g. `Saturday, September 19, 2026`).
    Full,
}

/// Time formatting style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeFormatStyle {
    /// Hours and minutes (e.g. `14:30` or `2:30 PM`).
    #[default]
    Short,
    /// Hours, minutes, and seconds (e.g. `14:30:00` or `2:30:00 PM`).
    Medium,
    /// Hours, minutes, seconds, and timezone offset if present.
    Long,
}

/// A calendar date (year, month, day) in the Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    /// Create and validate a calendar date.
    ///
    /// # Errors
    /// Returns `DateTimeError::InvalidMonth` if month is not in 1..=12.
    /// Returns `DateTimeError::InvalidDay` if day is not valid for the year and month.
    pub fn from_ymd(year: i32, month: u8, day: u8) -> Result<Self, DateTimeError> {
        if !(1..=12).contains(&month) {
            return Err(DateTimeError::InvalidMonth(month));
        }

        let max_days = days_in_month(year, month);
        if day == 0 || day > max_days {
            return Err(DateTimeError::InvalidDay {
                year,
                month,
                day,
                max_days,
            });
        }

        Ok(Self { year, month, day })
    }

    /// Year of the date.
    #[inline]
    #[must_use]
    pub const fn year(&self) -> i32 {
        self.year
    }

    /// Month of the date (1..=12).
    #[inline]
    #[must_use]
    pub const fn month(&self) -> u8 {
        self.month
    }

    /// Day of the month (1..=31).
    #[inline]
    #[must_use]
    pub const fn day(&self) -> u8 {
        self.day
    }

    /// Day of the week (0 = Monday, 1 = Tuesday, ..., 6 = Sunday).
    #[must_use]
    pub fn day_of_week(&self) -> u8 {
        compute_day_of_week(self.year, self.month, self.day)
    }

    /// Whether this date falls in a leap year.
    #[inline]
    #[must_use]
    pub const fn is_leap_year(&self) -> bool {
        is_leap_year(self.year)
    }
}

/// A wall-clock time representation (hours, minutes, seconds, milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Time {
    hour: u8,
    minute: u8,
    second: u8,
    millisecond: u16,
}

impl Time {
    /// Create and validate a time with second precision.
    ///
    /// # Errors
    /// Returns `DateTimeError` if any component is out of bounds.
    pub fn from_hms(hour: u8, minute: u8, second: u8) -> Result<Self, DateTimeError> {
        Self::from_hms_milli(hour, minute, second, 0)
    }

    /// Create and validate a time with millisecond precision.
    ///
    /// # Errors
    /// Returns `DateTimeError` if any component is out of bounds.
    pub fn from_hms_milli(
        hour: u8,
        minute: u8,
        second: u8,
        millisecond: u16,
    ) -> Result<Self, DateTimeError> {
        if hour > 23 {
            return Err(DateTimeError::InvalidHour(hour));
        }
        if minute > 59 {
            return Err(DateTimeError::InvalidMinute(minute));
        }
        if second > 59 {
            return Err(DateTimeError::InvalidSecond(second));
        }
        if millisecond > 999 {
            return Err(DateTimeError::InvalidMillisecond(millisecond));
        }

        Ok(Self {
            hour,
            minute,
            second,
            millisecond,
        })
    }

    /// Hour (0..=23).
    #[inline]
    #[must_use]
    pub const fn hour(&self) -> u8 {
        self.hour
    }

    /// Minute (0..=59).
    #[inline]
    #[must_use]
    pub const fn minute(&self) -> u8 {
        self.minute
    }

    /// Second (0..=59).
    #[inline]
    #[must_use]
    pub const fn second(&self) -> u8 {
        self.second
    }

    /// Millisecond (0..=999).
    #[inline]
    #[must_use]
    pub const fn millisecond(&self) -> u16 {
        self.millisecond
    }
}

/// Combined calendar date, time, and optional timezone offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DateTime {
    date: Date,
    time: Time,
    /// Timezone offset from UTC in minutes (e.g. +120 for UTC+2, -300 for UTC-5).
    timezone_offset_minutes: Option<i16>,
}

impl DateTime {
    /// Create a new DateTime from date and time without timezone offset.
    #[must_use]
    pub const fn new(date: Date, time: Time) -> Self {
        Self {
            date,
            time,
            timezone_offset_minutes: None,
        }
    }

    /// Create a new DateTime with date, time, and timezone offset in minutes.
    ///
    /// # Errors
    /// Returns `DateTimeError::InvalidTimezoneOffset` if offset is not in -720..=840.
    pub fn with_timezone_offset(
        date: Date,
        time: Time,
        offset_minutes: i16,
    ) -> Result<Self, DateTimeError> {
        if !(-720..=840).contains(&offset_minutes) {
            return Err(DateTimeError::InvalidTimezoneOffset(offset_minutes));
        }
        Ok(Self {
            date,
            time,
            timezone_offset_minutes: Some(offset_minutes),
        })
    }

    /// Convenience constructor from year, month, day, hour, minute, second.
    pub fn from_ymd_hms(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, DateTimeError> {
        let date = Date::from_ymd(year, month, day)?;
        let time = Time::from_hms(hour, minute, second)?;
        Ok(Self::new(date, time))
    }

    /// The date component.
    #[inline]
    #[must_use]
    pub const fn date(&self) -> &Date {
        &self.date
    }

    /// The time component.
    #[inline]
    #[must_use]
    pub const fn time(&self) -> &Time {
        &self.time
    }

    /// Timezone offset in minutes, if specified.
    #[inline]
    #[must_use]
    pub const fn timezone_offset_minutes(&self) -> Option<i16> {
        self.timezone_offset_minutes
    }
}

/// A locale-aware date and time formatter.
#[derive(Debug, Clone)]
pub struct DateTimeFormatter {
    locale_tag: String,
    symbols: DateSymbols,
}

impl DateTimeFormatter {
    /// Create a DateTimeFormatter for a locale tag.
    ///
    /// # Errors
    /// Returns `FormattingError::UnsupportedLocale` if the locale is not supported.
    pub fn for_locale(locale: &str) -> Result<Self, FormattingError> {
        let data = lookup_locale_data(locale)
            .ok_or_else(|| FormattingError::UnsupportedLocale(locale.to_string()))?;
        Ok(Self::from_data(data))
    }

    /// Create directly from pinned locale data.
    #[must_use]
    pub fn from_data(data: &'static LocaleData) -> Self {
        Self {
            locale_tag: data.tag.to_string(),
            symbols: data.date,
        }
    }

    /// Active locale tag.
    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale_tag
    }

    /// Format a calendar date according to the locale's conventions.
    #[must_use]
    pub fn format_date(&self, date: &Date, style: DateFormatStyle) -> String {
        let month_idx = (date.month() - 1) as usize;
        let month_abbr = self.symbols.months_abbr[month_idx];
        let month_full = self.symbols.months_full[month_idx];
        let dow_idx = date.day_of_week() as usize;
        let day_full = self.symbols.days_full[dow_idx];

        let base_tag = self.locale_tag.split(['-', '_']).next().unwrap_or("en");

        match base_tag {
            "en" => match style {
                DateFormatStyle::Short => {
                    format!("{:02}/{:02}/{:04}", date.month(), date.day(), date.year())
                }
                DateFormatStyle::Medium => {
                    format!("{} {}, {}", month_abbr, date.day(), date.year())
                }
                DateFormatStyle::Long => {
                    format!("{} {}, {}", month_full, date.day(), date.year())
                }
                DateFormatStyle::Full => {
                    format!(
                        "{}, {} {}, {}",
                        day_full,
                        month_full,
                        date.day(),
                        date.year()
                    )
                }
            },
            "de" => match style {
                DateFormatStyle::Short => {
                    format!("{:02}.{:02}.{:04}", date.day(), date.month(), date.year())
                }
                DateFormatStyle::Medium => {
                    format!("{}. {} {}", date.day(), month_abbr, date.year())
                }
                DateFormatStyle::Long => {
                    format!("{}. {} {}", date.day(), month_full, date.year())
                }
                DateFormatStyle::Full => {
                    format!(
                        "{}, {}. {} {}",
                        day_full,
                        date.day(),
                        month_full,
                        date.year()
                    )
                }
            },
            "fr" => match style {
                DateFormatStyle::Short => {
                    format!("{:02}/{:02}/{:04}", date.day(), date.month(), date.year())
                }
                DateFormatStyle::Medium => {
                    format!("{} {} {}", date.day(), month_abbr, date.year())
                }
                DateFormatStyle::Long => {
                    format!("{} {} {}", date.day(), month_full, date.year())
                }
                DateFormatStyle::Full => {
                    format!("{} {} {} {}", day_full, date.day(), month_full, date.year())
                }
            },
            "es" => match style {
                DateFormatStyle::Short => {
                    format!("{:02}/{:02}/{:04}", date.day(), date.month(), date.year())
                }
                DateFormatStyle::Medium => {
                    format!("{} {} {}", date.day(), month_abbr, date.year())
                }
                DateFormatStyle::Long => {
                    format!("{} de {} de {}", date.day(), month_full, date.year())
                }
                DateFormatStyle::Full => {
                    format!(
                        "{}, {} de {} de {}",
                        day_full,
                        date.day(),
                        month_full,
                        date.year()
                    )
                }
            },
            "ru" => match style {
                DateFormatStyle::Short => {
                    format!("{:02}.{:02}.{:04}", date.day(), date.month(), date.year())
                }
                DateFormatStyle::Medium => {
                    format!("{} {} {} г.", date.day(), month_abbr, date.year())
                }
                DateFormatStyle::Long => {
                    format!("{} {} {} г.", date.day(), month_full, date.year())
                }
                DateFormatStyle::Full => {
                    format!(
                        "{}, {} {} {} г.",
                        day_full,
                        date.day(),
                        month_full,
                        date.year()
                    )
                }
            },
            "ar" => match style {
                DateFormatStyle::Short => {
                    format!("{:02}/{:02}/{:04}", date.day(), date.month(), date.year())
                }
                DateFormatStyle::Medium => {
                    format!("{} {}، {}", date.day(), month_abbr, date.year())
                }
                DateFormatStyle::Long => {
                    format!("{} {} {}", date.day(), month_full, date.year())
                }
                DateFormatStyle::Full => {
                    format!(
                        "{}، {} {} {}",
                        day_full,
                        date.day(),
                        month_full,
                        date.year()
                    )
                }
            },
            "ja" => match style {
                DateFormatStyle::Short | DateFormatStyle::Medium => {
                    format!("{:04}/{:02}/{:02}", date.year(), date.month(), date.day())
                }
                DateFormatStyle::Long => {
                    format!("{}年{}月{}日", date.year(), date.month(), date.day())
                }
                DateFormatStyle::Full => {
                    format!(
                        "{}年{}月{}日 {}",
                        date.year(),
                        date.month(),
                        date.day(),
                        day_full
                    )
                }
            },
            _ => {
                // Default fallback pattern (ISO 8601 style)
                format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
            }
        }
    }

    /// Format a wall-clock time according to the locale's conventions.
    #[must_use]
    pub fn format_time(&self, time: &Time, style: TimeFormatStyle) -> String {
        let (hour_str, am_pm_str) = if self.symbols.is_24h {
            (format!("{:02}", time.hour()), None)
        } else {
            let (h12, am_pm) = if time.hour() == 0 {
                (12, self.symbols.am_pm.0)
            } else if time.hour() < 12 {
                (time.hour(), self.symbols.am_pm.0)
            } else if time.hour() == 12 {
                (12, self.symbols.am_pm.1)
            } else {
                (time.hour() - 12, self.symbols.am_pm.1)
            };
            (h12.to_string(), Some(am_pm))
        };

        match style {
            TimeFormatStyle::Short => {
                if let Some(ap) = am_pm_str {
                    format!("{}:{:02} {}", hour_str, time.minute(), ap)
                } else {
                    format!("{}:{:02}", hour_str, time.minute())
                }
            }
            TimeFormatStyle::Medium | TimeFormatStyle::Long => {
                if let Some(ap) = am_pm_str {
                    format!(
                        "{}:{:02}:{:02} {}",
                        hour_str,
                        time.minute(),
                        time.second(),
                        ap
                    )
                } else {
                    format!("{}:{:02}:{:02}", hour_str, time.minute(), time.second())
                }
            }
        }
    }

    /// Format a combined DateTime.
    #[must_use]
    pub fn format_datetime(
        &self,
        dt: &DateTime,
        date_style: DateFormatStyle,
        time_style: TimeFormatStyle,
    ) -> String {
        let date_str = self.format_date(dt.date(), date_style);
        let mut time_str = self.format_time(dt.time(), time_style);

        if time_style == TimeFormatStyle::Long
            && let Some(offset) = dt.timezone_offset_minutes()
        {
            time_str.push(' ');
            time_str.push_str(&format_tz_offset(offset));
        }

        let separator = self.symbols.datetime_separator;
        match self.symbols.datetime_order {
            DateTimeOrder::DateThenTime => format!("{date_str}{separator}{time_str}"),
            DateTimeOrder::TimeThenDate => format!("{time_str}{separator}{date_str}"),
        }
    }
}

/// Helper to format timezone offset in minutes as `+HH:MM` or `-HH:MM` (or `Z`).
fn format_tz_offset(offset_minutes: i16) -> String {
    if offset_minutes == 0 {
        return "UTC".to_string();
    }
    let sign = if offset_minutes >= 0 { '+' } else { '-' };
    let abs_mins = offset_minutes.unsigned_abs();
    let hours = abs_mins / 60;
    let mins = abs_mins % 60;
    format!("UTC{}{:02}:{:02}", sign, hours, mins)
}

/// Calculate whether a year is a leap year in the Gregorian calendar.
#[inline]
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Calculate the number of days in a month for a given year.
pub const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Compute day of week (0 = Monday, 1 = Tuesday, ..., 6 = Sunday).
///
/// Sakamoto's method, evaluated in `i64` with Euclidean division. Neither of
/// those is cosmetic:
///
/// - `i32` overflows. [`Date::from_ymd`] validates the month and the day but
///   accepts *any* `i32` year, so `from_ymd(i32::MAX, 3, 1)` is a perfectly
///   well-formed `Date` - and `y + y / 4` on it exceeds `i32::MAX`, which is a
///   panic in a debug build (and a wrapped, nonsense weekday in release).
///   Widening covers the whole `i32` domain the constructor admits.
/// - Rust's `/` truncates toward zero, but the century corrections are
///   leap-day *counts* and need the floor. The two agree above zero and
///   diverge below it, where truncation placed 0000-01-01 on a Sunday; the
///   proleptic Gregorian calendar has it on a Saturday. `div_euclid` is the
///   floor, and `rem_euclid` then keeps the result non-negative so the ISO
///   rotation below cannot yield a negative index for the weekday tables.
fn compute_day_of_week(y: i32, m: u8, d: u8) -> u8 {
    const T: [i64; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = i64::from(y);
    let y_adj = if m < 3 { y - 1 } else { y };
    let dow = (y_adj + y_adj.div_euclid(4) - y_adj.div_euclid(100)
        + y_adj.div_euclid(400)
        + T[(m - 1) as usize]
        + i64::from(d))
    .rem_euclid(7);
    // dow: 0 = Sun, 1 = Mon, 2 = Tue, 3 = Wed, 4 = Thu, 5 = Fri, 6 = Sat
    // Convert to ISO: 0 = Mon, 1 = Tue, 2 = Wed, 3 = Thu, 4 = Fri, 5 = Sat, 6 = Sun
    let dow_iso = (dow + 6) % 7;
    dow_iso as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leap_years() {
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2023));
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2028));
    }

    #[test]
    fn test_days_in_month() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 4), 30);
    }

    #[test]
    fn test_date_validation() {
        assert!(Date::from_ymd(2024, 2, 29).is_ok());
        assert!(Date::from_ymd(2023, 2, 29).is_err());
        assert!(Date::from_ymd(2026, 13, 1).is_err());
        assert!(Date::from_ymd(2026, 0, 1).is_err());
        assert!(Date::from_ymd(2026, 1, 32).is_err());
    }

    #[test]
    fn test_time_validation() {
        assert!(Time::from_hms(23, 59, 59).is_ok());
        assert!(Time::from_hms(24, 0, 0).is_err());
        assert!(Time::from_hms(12, 60, 0).is_err());
        assert!(Time::from_hms(12, 0, 60).is_err());
    }

    #[test]
    fn test_day_of_week() {
        // 2026-09-19 is a Saturday (ISO 5: Mon=0, Tue=1, Wed=2, Thu=3, Fri=4, Sat=5, Sun=6)
        let date = Date::from_ymd(2026, 9, 19).unwrap();
        assert_eq!(date.day_of_week(), 5);

        // 2026-09-20 is Sunday (ISO 6)
        let sun = Date::from_ymd(2026, 9, 20).unwrap();
        assert_eq!(sun.day_of_week(), 6);

        // 2026-09-21 is Monday (ISO 0)
        let mon = Date::from_ymd(2026, 9, 21).unwrap();
        assert_eq!(mon.day_of_week(), 0);
    }

    #[test]
    fn day_of_week_spans_every_year_from_ymd_accepts() {
        // `from_ymd` validates month and day but takes any `i32` year, so
        // both of these are well-formed `Date`s that a caller can build. In
        // `i32`, `y + y / 4` on the first one overflows - a debug-build panic
        // reachable from safe, validated input.
        for date in [
            Date::from_ymd(i32::MAX, 3, 1).unwrap(),
            Date::from_ymd(i32::MAX, 1, 1).unwrap(),
            Date::from_ymd(i32::MIN, 1, 1).unwrap(),
            Date::from_ymd(i32::MIN, 12, 31).unwrap(),
        ] {
            // In range means the weekday tables can be indexed by it; out of
            // range means `format_date(.., Full)` panics.
            assert!(date.day_of_week() <= 6, "{date:?}");
            let fmt = DateTimeFormatter::for_locale("en").unwrap();
            assert!(!fmt.format_date(&date, DateFormatStyle::Full).is_empty());
        }
    }

    #[test]
    fn day_of_week_uses_floor_division_below_year_one() {
        // Sakamoto's century terms count leap days, so they need the floor,
        // not Rust's truncation toward zero. The two agree above zero and
        // diverge below it.
        //
        // Anchor: the Gregorian 400-year cycle is 146097 days, which is
        // exactly 20871 weeks, so 0000-03-01 and 2000-03-01 are the same
        // weekday - a Wednesday. Every value below is that anchor walked
        // forwards or backwards by a known number of days.
        assert_eq!(Date::from_ymd(2000, 3, 1).unwrap().day_of_week(), 2); // Wed
        assert_eq!(Date::from_ymd(0, 3, 1).unwrap().day_of_week(), 2); // Wed

        // Year 0 is a leap year, so Jan 1 is 60 days before Mar 1: Saturday.
        // Truncating division reported Sunday here.
        assert_eq!(Date::from_ymd(0, 1, 1).unwrap().day_of_week(), 5); // Sat

        // Year 0 then runs 366 days (2 mod 7) to a Monday, the conventional
        // weekday for 0001-01-01 proleptic Gregorian.
        assert_eq!(Date::from_ymd(1, 1, 1).unwrap().day_of_week(), 0); // Mon

        // Year -1 is not a leap year, so it runs 365 days (1 mod 7) up to
        // that Saturday, putting its own Jan 1 on a Friday.
        assert_eq!(Date::from_ymd(-1, 1, 1).unwrap().day_of_week(), 4); // Fri
    }

    #[test]
    fn test_date_formatting_styles() {
        let date = Date::from_ymd(2026, 9, 19).unwrap();

        let fmt_en = DateTimeFormatter::for_locale("en").unwrap();
        assert_eq!(
            fmt_en.format_date(&date, DateFormatStyle::Short),
            "09/19/2026"
        );
        assert_eq!(
            fmt_en.format_date(&date, DateFormatStyle::Medium),
            "Sep 19, 2026"
        );
        assert_eq!(
            fmt_en.format_date(&date, DateFormatStyle::Long),
            "September 19, 2026"
        );
        assert_eq!(
            fmt_en.format_date(&date, DateFormatStyle::Full),
            "Saturday, September 19, 2026"
        );

        let fmt_de = DateTimeFormatter::for_locale("de").unwrap();
        assert_eq!(
            fmt_de.format_date(&date, DateFormatStyle::Short),
            "19.09.2026"
        );
        assert_eq!(
            fmt_de.format_date(&date, DateFormatStyle::Medium),
            "19. Sept. 2026"
        );
        assert_eq!(
            fmt_de.format_date(&date, DateFormatStyle::Long),
            "19. September 2026"
        );
        assert_eq!(
            fmt_de.format_date(&date, DateFormatStyle::Full),
            "Samstag, 19. September 2026"
        );

        let fmt_ja = DateTimeFormatter::for_locale("ja").unwrap();
        assert_eq!(
            fmt_ja.format_date(&date, DateFormatStyle::Short),
            "2026/09/19"
        );
        assert_eq!(
            fmt_ja.format_date(&date, DateFormatStyle::Long),
            "2026年9月19日"
        );
        assert_eq!(
            fmt_ja.format_date(&date, DateFormatStyle::Full),
            "2026年9月19日 土曜日"
        );
    }

    #[test]
    fn test_time_formatting_styles() {
        let t_afternoon = Time::from_hms(14, 30, 45).unwrap();
        let t_morning = Time::from_hms(9, 5, 0).unwrap();

        // 12-hour locale (en)
        let fmt_en = DateTimeFormatter::for_locale("en").unwrap();
        assert_eq!(
            fmt_en.format_time(&t_afternoon, TimeFormatStyle::Short),
            "2:30 PM"
        );
        assert_eq!(
            fmt_en.format_time(&t_afternoon, TimeFormatStyle::Medium),
            "2:30:45 PM"
        );
        assert_eq!(
            fmt_en.format_time(&t_morning, TimeFormatStyle::Short),
            "9:05 AM"
        );

        // 24-hour locale (de)
        let fmt_de = DateTimeFormatter::for_locale("de").unwrap();
        assert_eq!(
            fmt_de.format_time(&t_afternoon, TimeFormatStyle::Short),
            "14:30"
        );
        assert_eq!(
            fmt_de.format_time(&t_afternoon, TimeFormatStyle::Medium),
            "14:30:45"
        );
        assert_eq!(
            fmt_de.format_time(&t_morning, TimeFormatStyle::Short),
            "09:05"
        );
    }

    #[test]
    fn test_combined_datetime() {
        let dt = DateTime::from_ymd_hms(2026, 9, 19, 14, 30, 0).unwrap();

        let fmt_en = DateTimeFormatter::for_locale("en").unwrap();
        assert_eq!(
            fmt_en.format_datetime(&dt, DateFormatStyle::Short, TimeFormatStyle::Short),
            "09/19/2026, 2:30 PM"
        );

        let fmt_de = DateTimeFormatter::for_locale("de").unwrap();
        assert_eq!(
            fmt_de.format_datetime(&dt, DateFormatStyle::Short, TimeFormatStyle::Short),
            "19.09.2026, 14:30"
        );
    }

    #[test]
    fn combined_datetime_follows_the_locale_order() {
        // `DateSymbols::datetime_order` is public data that `from_data`
        // accepts, and it was ignored: time-first data still put date first.
        let en = lookup_locale_data("en").unwrap();
        let time_first: &'static LocaleData = Box::leak(Box::new(LocaleData {
            date: DateSymbols {
                datetime_order: DateTimeOrder::TimeThenDate,
                ..en.date
            },
            ..*en
        }));
        let dt = DateTime::from_ymd_hms(2026, 9, 19, 14, 30, 0).unwrap();
        assert_eq!(
            DateTimeFormatter::from_data(time_first).format_datetime(
                &dt,
                DateFormatStyle::Short,
                TimeFormatStyle::Short
            ),
            "2:30 PM, 09/19/2026"
        );
    }

    #[test]
    fn test_timezone_formatting() {
        let date = Date::from_ymd(2026, 9, 19).unwrap();
        let time = Time::from_hms(14, 30, 0).unwrap();
        let dt_tz = DateTime::with_timezone_offset(date, time, 120).unwrap();

        let fmt = DateTimeFormatter::for_locale("en").unwrap();
        let formatted = fmt.format_datetime(&dt_tz, DateFormatStyle::Short, TimeFormatStyle::Long);
        assert_eq!(formatted, "09/19/2026, 2:30:00 PM UTC+02:00");
    }
}
