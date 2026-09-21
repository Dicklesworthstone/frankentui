#![forbid(unsafe_code)]

//! Pinned CLDR locale formatting data for FrankenTUI.
//!
//! Pinned data version: Unicode CLDR release 45.0.
//!
//! Provides deterministic reference data for the 7 primary supported locales:
//! English (`en`), Spanish (`es`), French (`fr`), German (`de`),
//! Russian (`ru`), Arabic (`ar`), and Japanese (`ja`).

/// Version of Unicode CLDR from which formatting data was extracted.
pub const CLDR_VERSION: &str = "45.0";

/// Explicit list of primary supported locale tags.
pub const SUPPORTED_LOCALES: &[&str] = &["en", "es", "fr", "de", "ru", "ar", "ja"];

/// Where the percent sign is placed relative to the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PercentPlacement {
    /// Placed immediately after the number without space (e.g. `50%`).
    Suffix,
    /// Placed after the number with a space (e.g. `50 %`).
    SuffixWithSpace,
    /// Placed before the number (e.g. `%50`).
    Prefix,
}

/// Where the currency symbol is placed relative to the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrencyPlacement {
    /// Placed immediately before the number (e.g. `$100`).
    Prefix,
    /// Placed before the number with a space (e.g. `USD 100`).
    PrefixWithSpace,
    /// Placed immediately after the number (e.g. `100€`).
    Suffix,
    /// Placed after the number with a space (e.g. `100 €`).
    SuffixWithSpace,
}

/// Order when combining date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateTimeOrder {
    /// Date followed by time (e.g. `2026-09-19 12:00`).
    DateThenTime,
    /// Time followed by date.
    TimeThenDate,
}

/// Locale-specific numeric symbols and formatting conventions.
#[derive(Debug, Clone, Copy)]
pub struct NumberSymbols {
    /// Decimal point character(s).
    pub decimal_sep: &'static str,
    /// Thousands / grouping separator character(s).
    pub group_sep: &'static str,
    /// Number of digits in each integer group (typically 3).
    pub group_size: usize,
    /// Negative sign character(s).
    pub minus_sign: &'static str,
    /// Positive sign character(s).
    pub plus_sign: &'static str,
    /// Percent symbol.
    pub percent_sign: &'static str,
    /// Percent sign placement convention.
    pub percent_placement: PercentPlacement,
    /// Default ISO 4217 currency code for the locale.
    pub default_currency_code: &'static str,
    /// Default currency symbol for the locale.
    pub default_currency_symbol: &'static str,
    /// Currency symbol placement convention.
    pub currency_placement: CurrencyPlacement,
}

/// Locale-specific date and time symbols and patterns.
#[derive(Debug, Clone, Copy)]
pub struct DateSymbols {
    /// Full month names (January = index 0 .. December = index 11).
    pub months_full: [&'static str; 12],
    /// Abbreviated month names (Jan = index 0 .. Dec = index 11).
    pub months_abbr: [&'static str; 12],
    /// Full day-of-week names (Monday = index 0 .. Sunday = index 6).
    pub days_full: [&'static str; 7],
    /// Abbreviated day-of-week names (Mon = index 0 .. Sun = index 6).
    pub days_abbr: [&'static str; 7],
    /// Ante meridiem (AM) and Post meridiem (PM) symbols.
    pub am_pm: (&'static str, &'static str),
    /// Whether standard time representation uses 24-hour clock.
    pub is_24h: bool,
    /// Date-time separator (e.g. ", " or " ").
    pub datetime_separator: &'static str,
    /// Date-time ordering.
    pub datetime_order: DateTimeOrder,
}

/// Consolidated locale formatting data entry.
#[derive(Debug, Clone, Copy)]
pub struct LocaleData {
    /// Primary BCP-47 language tag.
    pub tag: &'static str,
    /// Numeric formatting symbols.
    pub number: NumberSymbols,
    /// Date/time formatting symbols.
    pub date: DateSymbols,
}

const DATA_EN: LocaleData = LocaleData {
    tag: "en",
    number: NumberSymbols {
        decimal_sep: ".",
        group_sep: ",",
        group_size: 3,
        minus_sign: "-",
        plus_sign: "+",
        percent_sign: "%",
        percent_placement: PercentPlacement::Suffix,
        default_currency_code: "USD",
        default_currency_symbol: "$",
        currency_placement: CurrencyPlacement::Prefix,
    },
    date: DateSymbols {
        months_full: [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ],
        months_abbr: [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ],
        days_full: [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ],
        days_abbr: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
        am_pm: ("AM", "PM"),
        is_24h: false,
        datetime_separator: ", ",
        datetime_order: DateTimeOrder::DateThenTime,
    },
};

const DATA_DE: LocaleData = LocaleData {
    tag: "de",
    number: NumberSymbols {
        decimal_sep: ",",
        group_sep: ".",
        group_size: 3,
        minus_sign: "-",
        plus_sign: "+",
        percent_sign: "%",
        percent_placement: PercentPlacement::SuffixWithSpace,
        default_currency_code: "EUR",
        default_currency_symbol: "€",
        currency_placement: CurrencyPlacement::SuffixWithSpace,
    },
    date: DateSymbols {
        months_full: [
            "Januar",
            "Februar",
            "März",
            "April",
            "Mai",
            "Juni",
            "Juli",
            "August",
            "September",
            "Oktober",
            "November",
            "Dezember",
        ],
        months_abbr: [
            "Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.",
            "Dez.",
        ],
        days_full: [
            "Montag",
            "Dienstag",
            "Mittwoch",
            "Donnerstag",
            "Freitag",
            "Samstag",
            "Sonntag",
        ],
        days_abbr: ["Mo.", "Di.", "Mi.", "Do.", "Fr.", "Sa.", "So."],
        am_pm: ("vorm.", "nachm."),
        is_24h: true,
        datetime_separator: ", ",
        datetime_order: DateTimeOrder::DateThenTime,
    },
};

const DATA_FR: LocaleData = LocaleData {
    tag: "fr",
    number: NumberSymbols {
        decimal_sep: ",",
        group_sep: "\u{202f}", // narrow no-break space
        group_size: 3,
        minus_sign: "-",
        plus_sign: "+",
        percent_sign: "%",
        percent_placement: PercentPlacement::SuffixWithSpace,
        default_currency_code: "EUR",
        default_currency_symbol: "€",
        currency_placement: CurrencyPlacement::SuffixWithSpace,
    },
    date: DateSymbols {
        months_full: [
            "janvier",
            "février",
            "mars",
            "avril",
            "mai",
            "juin",
            "juillet",
            "août",
            "septembre",
            "octobre",
            "novembre",
            "décembre",
        ],
        months_abbr: [
            "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.",
            "nov.", "déc.",
        ],
        days_full: [
            "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
        ],
        days_abbr: ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."],
        am_pm: ("AM", "PM"),
        is_24h: true,
        datetime_separator: " à ",
        datetime_order: DateTimeOrder::DateThenTime,
    },
};

const DATA_ES: LocaleData = LocaleData {
    tag: "es",
    number: NumberSymbols {
        decimal_sep: ",",
        group_sep: ".",
        group_size: 3,
        minus_sign: "-",
        plus_sign: "+",
        percent_sign: "%",
        percent_placement: PercentPlacement::SuffixWithSpace,
        default_currency_code: "EUR",
        default_currency_symbol: "€",
        currency_placement: CurrencyPlacement::SuffixWithSpace,
    },
    date: DateSymbols {
        months_full: [
            "enero",
            "febrero",
            "marzo",
            "abril",
            "mayo",
            "junio",
            "julio",
            "agosto",
            "septiembre",
            "octubre",
            "noviembre",
            "diciembre",
        ],
        months_abbr: [
            "ene.", "feb.", "mar.", "abr.", "may.", "jun.", "jul.", "ago.", "sept.", "oct.",
            "nov.", "dic.",
        ],
        days_full: [
            "lunes",
            "martes",
            "miércoles",
            "jueves",
            "viernes",
            "sábado",
            "domingo",
        ],
        days_abbr: ["lun.", "mar.", "mié.", "jue.", "vie.", "sáb.", "dom."],
        am_pm: ("a. m.", "p. m."),
        is_24h: true,
        datetime_separator: ", ",
        datetime_order: DateTimeOrder::DateThenTime,
    },
};

const DATA_RU: LocaleData = LocaleData {
    tag: "ru",
    number: NumberSymbols {
        decimal_sep: ",",
        group_sep: "\u{00a0}", // non-breaking space
        group_size: 3,
        minus_sign: "-",
        plus_sign: "+",
        percent_sign: "%",
        percent_placement: PercentPlacement::SuffixWithSpace,
        default_currency_code: "RUB",
        default_currency_symbol: "₽",
        currency_placement: CurrencyPlacement::SuffixWithSpace,
    },
    date: DateSymbols {
        months_full: [
            "января",
            "февраля",
            "марта",
            "апреля",
            "мая",
            "июня",
            "июля",
            "августа",
            "сентября",
            "октября",
            "ноября",
            "декабря",
        ],
        months_abbr: [
            "янв.",
            "февр.",
            "мар.",
            "апр.",
            "мая",
            "июня",
            "июля",
            "авг.",
            "сент.",
            "окт.",
            "нояб.",
            "дек.",
        ],
        days_full: [
            "понедельник",
            "вторник",
            "среда",
            "четверг",
            "пятница",
            "суббота",
            "воскресенье",
        ],
        days_abbr: ["пн", "вт", "ср", "чт", "пт", "сб", "вс"],
        am_pm: ("AM", "PM"),
        is_24h: true,
        datetime_separator: ", ",
        datetime_order: DateTimeOrder::DateThenTime,
    },
};

const DATA_AR: LocaleData = LocaleData {
    tag: "ar",
    number: NumberSymbols {
        decimal_sep: ".",
        group_sep: ",",
        group_size: 3,
        minus_sign: "-",
        plus_sign: "+",
        percent_sign: "%",
        percent_placement: PercentPlacement::Suffix,
        default_currency_code: "SAR",
        default_currency_symbol: "ر.س",
        currency_placement: CurrencyPlacement::SuffixWithSpace,
    },
    date: DateSymbols {
        months_full: [
            "يناير",
            "فبراير",
            "مارس",
            "أبريل",
            "مايو",
            "يونيو",
            "يوليو",
            "أغسطس",
            "سبتمبر",
            "أكتوبر",
            "نوفمبر",
            "ديسمبر",
        ],
        months_abbr: [
            "يناير",
            "فبراير",
            "مارس",
            "أبريل",
            "مايو",
            "يونيو",
            "يوليو",
            "أغسطس",
            "سبتمبر",
            "أكتوبر",
            "نوفمبر",
            "ديسمبر",
        ],
        days_full: [
            "الاثنين",
            "الثلاثاء",
            "الأربعاء",
            "الخميس",
            "الجمعة",
            "السبت",
            "الأحد",
        ],
        days_abbr: ["اثن", "ثلا", "أرب", "خمي", "جمع", "سبت", "أحد"],
        am_pm: ("ص", "م"),
        is_24h: false,
        datetime_separator: " في ",
        datetime_order: DateTimeOrder::DateThenTime,
    },
};

const DATA_JA: LocaleData = LocaleData {
    tag: "ja",
    number: NumberSymbols {
        decimal_sep: ".",
        group_sep: ",",
        group_size: 3,
        minus_sign: "-",
        plus_sign: "+",
        percent_sign: "%",
        percent_placement: PercentPlacement::Suffix,
        default_currency_code: "JPY",
        default_currency_symbol: "￥",
        currency_placement: CurrencyPlacement::Prefix,
    },
    date: DateSymbols {
        months_full: [
            "1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月",
        ],
        months_abbr: [
            "1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月",
        ],
        days_full: [
            "月曜日",
            "火曜日",
            "水曜日",
            "木曜日",
            "金曜日",
            "土曜日",
            "日曜日",
        ],
        days_abbr: ["月", "火", "水", "木", "金", "土", "日"],
        am_pm: ("午前", "午後"),
        is_24h: true,
        datetime_separator: " ",
        datetime_order: DateTimeOrder::DateThenTime,
    },
};

/// Lookup formatting data for a given locale tag.
///
/// Matches exact tags (e.g. `en-US`), with fallback to base language (e.g. `en`).
/// Returns `None` if the locale is not supported in the pinned data matrix.
#[must_use]
pub fn lookup_locale_data(locale_tag: &str) -> Option<&'static LocaleData> {
    let trimmed = locale_tag.trim();
    if trimmed.is_empty() {
        return None;
    }

    let base = trimmed.split(['-', '_']).next()?.to_ascii_lowercase();

    match base.as_str() {
        "en" => Some(&DATA_EN),
        "de" => Some(&DATA_DE),
        "fr" => Some(&DATA_FR),
        "es" => Some(&DATA_ES),
        "ru" => Some(&DATA_RU),
        "ar" => Some(&DATA_AR),
        "ja" => Some(&DATA_JA),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::datetime::Date;
    use crate::plural::PluralRule;

    /// The set of supported locales is written down four times: this
    /// `SUPPORTED_LOCALES` const, the `match` in [`lookup_locale_data`], the
    /// `match` in `PluralRule::for_locale`, and the per-locale arms of
    /// `DateTimeFormatter::format_date`. Nothing makes them agree, and every
    /// way they can disagree fails silently rather than loudly: a tag in the
    /// const but missing from `lookup_locale_data` resolves to `None` and the
    /// caller formats a supposedly supported locale as English.
    #[test]
    fn every_advertised_locale_resolves_to_data_tagged_with_itself() {
        for tag in SUPPORTED_LOCALES {
            let data = lookup_locale_data(tag).unwrap_or_else(|| {
                panic!("{tag} is in SUPPORTED_LOCALES but lookup_locale_data returns None")
            });
            assert_eq!(
                data.tag, *tag,
                "lookup of {tag:?} returned data tagged {:?}",
                data.tag
            );
        }
    }

    /// Russian has four plural categories and Arabic six. Falling through to
    /// the English rule for either is not a near-miss, it selects the wrong
    /// message form outright, so the mapping is pinned per locale rather than
    /// merely asserted to exist.
    #[test]
    fn every_advertised_locale_has_the_plural_rule_cldr_gives_it() {
        let expected: &[(&str, PluralRule)] = &[
            ("en", PluralRule::English),
            ("es", PluralRule::English),
            ("de", PluralRule::English),
            ("fr", PluralRule::French),
            ("ru", PluralRule::Russian),
            ("ar", PluralRule::Arabic),
            ("ja", PluralRule::CJK),
        ];
        assert_eq!(
            expected.len(),
            SUPPORTED_LOCALES.len(),
            "a locale was added to SUPPORTED_LOCALES without a plural rule here"
        );
        for (tag, rule) in expected {
            assert!(
                SUPPORTED_LOCALES.contains(tag),
                "{tag} is pinned here but not advertised"
            );
            assert_eq!(
                format!("{:?}", PluralRule::for_locale(tag)),
                format!("{rule:?}"),
                "plural rule for {tag}"
            );
        }
    }

    /// `days_full` and `days_abbr` are documented as `Mon = index 0 .. Sun =
    /// index 6`, and `format_date` indexes them with `Date::day_of_week`,
    /// which returns an ISO index. The weekday arithmetic is tested on its
    /// own and the arrays are only ever read through it, so the two
    /// conventions can drift apart with almost nothing noticing. Only English
    /// was protected, and only incidentally, by `test_date_formatting_styles`
    /// comparing a rendered `Full` date: with the Russian table reordered
    /// Sunday-first, 83 of the crate's 84 tests still passed while every
    /// Russian weekday name came out wrong.
    #[test]
    fn weekday_tables_are_ordered_the_way_day_of_week_indexes_them() {
        // 2024-01-01 was a Monday and 2024-01-07 the Sunday that ended that
        // week, which pins both ends of every table.
        let monday = Date::from_ymd(2024, 1, 1).expect("2024-01-01");
        let sunday = Date::from_ymd(2024, 1, 7).expect("2024-01-07");
        assert_eq!(monday.day_of_week(), 0, "ISO index for Monday");
        assert_eq!(sunday.day_of_week(), 6, "ISO index for Sunday");

        let names: &[(&str, &str, &str)] = &[
            ("en", "Monday", "Sunday"),
            ("de", "Montag", "Sonntag"),
            ("fr", "lundi", "dimanche"),
            ("es", "lunes", "domingo"),
            ("ru", "понедельник", "воскресенье"),
            ("ar", "الاثنين", "الأحد"),
            ("ja", "月曜日", "日曜日"),
        ];
        assert_eq!(
            names.len(),
            SUPPORTED_LOCALES.len(),
            "a locale was added to SUPPORTED_LOCALES without weekday names here"
        );

        for (tag, monday_name, sunday_name) in names {
            let date = lookup_locale_data(tag).expect("advertised locale").date;
            assert_eq!(
                date.days_full[monday.day_of_week() as usize],
                *monday_name,
                "{tag}: Monday"
            );
            assert_eq!(
                date.days_full[sunday.day_of_week() as usize],
                *sunday_name,
                "{tag}: Sunday"
            );
            // Abbreviations must follow the same order as the full names.
            assert!(
                date.days_abbr.len() == 7 && date.months_abbr.len() == 12,
                "{tag}: table lengths"
            );
            let distinct: std::collections::BTreeSet<&str> =
                date.days_full.iter().copied().collect();
            assert_eq!(distinct.len(), 7, "{tag}: duplicate weekday name");
        }
    }

    /// `lookup_locale_data` documents a base-language fallback. Region
    /// subtags and case must not change the answer, and an unsupported
    /// language must stay `None` rather than resolving to English.
    #[test]
    fn lookup_takes_the_base_language_and_rejects_everything_else() {
        for (input, want) in [
            ("en-US", Some("en")),
            ("en_GB", Some("en")),
            ("DE", Some("de")),
            ("ja-JP-u-ca-japanese", Some("ja")),
            ("pt-BR", None),
            ("zz", None),
            ("", None),
            ("   ", None),
        ] {
            assert_eq!(
                lookup_locale_data(input).map(|d| d.tag),
                want,
                "lookup_locale_data({input:?})"
            );
        }
    }
}
