#![forbid(unsafe_code)]

//! Locale-aware number, currency, and percentage formatting.

use super::data::{
    CurrencyPlacement, LocaleData, NumberSymbols, PercentPlacement, lookup_locale_data,
};
use super::error::FormattingError;

/// Rounding strategy for numeric formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoundingMode {
    /// Round half away from zero (standard commercial / school rounding).
    #[default]
    HalfUp,
    /// Round half to nearest even digit (banker's rounding).
    HalfEven,
    /// Round towards zero (truncate).
    Truncate,
    /// Round towards negative infinity (floor).
    Floor,
    /// Round towards positive infinity (ceil).
    Ceil,
}

/// Numerical display style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumberStyle {
    /// Standard decimal format (e.g. `1,234.56`).
    #[default]
    Decimal,
    /// Percentage format multiplied by 100 with percent sign (e.g. `12.5%`).
    Percent,
    /// Currency format with symbol/code (e.g. `$1,234.56` or `1.234,56 €`).
    Currency {
        /// Optional custom currency code (e.g. "USD", "EUR").
        code: Option<&'static str>,
        /// Optional custom currency symbol (e.g. "$", "€").
        symbol: Option<&'static str>,
    },
}

/// Digit numbering system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumberingSystem {
    /// Latin digits 0-9 (ASCII).
    #[default]
    Latn,
    /// Eastern Arabic digits ٠-٩ (U+0660..=U+0669).
    Arab,
}

/// Configuration for number formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberFormat {
    /// Minimum number of integer digits (left-padded with zeros).
    pub min_integer_digits: usize,
    /// Minimum number of fractional digits (right-padded with zeros).
    pub min_fraction_digits: usize,
    /// Maximum number of fractional digits (rounded).
    pub max_fraction_digits: usize,
    /// Whether thousands / group separators should be inserted.
    pub use_grouping: bool,
    /// Rounding mode when truncating fractional digits.
    pub rounding_mode: RoundingMode,
    /// Display style (Decimal, Percent, Currency).
    pub style: NumberStyle,
    /// Numbering system / digit script.
    pub numbering_system: NumberingSystem,
    /// Whether non-finite floating point numbers (NaN, Inf) should be formatted
    /// as symbols instead of returning `FormattingError::NonFinite`.
    pub allow_non_finite: bool,
}

impl Default for NumberFormat {
    fn default() -> Self {
        Self {
            min_integer_digits: 1,
            min_fraction_digits: 0,
            max_fraction_digits: 3,
            use_grouping: true,
            rounding_mode: RoundingMode::HalfUp,
            style: NumberStyle::Decimal,
            numbering_system: NumberingSystem::Latn,
            allow_non_finite: false,
        }
    }
}

impl NumberFormat {
    /// Create a new decimal number format configuration with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set minimum integer digits.
    #[must_use]
    pub const fn min_integer_digits(mut self, digits: usize) -> Self {
        self.min_integer_digits = digits;
        self
    }

    /// Set fraction digit bounds.
    #[must_use]
    pub const fn fraction_digits(mut self, min: usize, max: usize) -> Self {
        self.min_fraction_digits = min;
        self.max_fraction_digits = max;
        self
    }

    /// Set whether grouping separators are used.
    #[must_use]
    pub const fn use_grouping(mut self, grouping: bool) -> Self {
        self.use_grouping = grouping;
        self
    }

    /// Set the rounding mode.
    #[must_use]
    pub const fn rounding_mode(mut self, mode: RoundingMode) -> Self {
        self.rounding_mode = mode;
        self
    }

    /// Set the number style.
    #[must_use]
    pub const fn style(mut self, style: NumberStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the numbering system.
    #[must_use]
    pub const fn numbering_system(mut self, system: NumberingSystem) -> Self {
        self.numbering_system = system;
        self
    }

    /// Set whether non-finite numbers (NaN, Inf) format to strings instead of erroring.
    #[must_use]
    pub const fn allow_non_finite(mut self, allow: bool) -> Self {
        self.allow_non_finite = allow;
        self
    }
}

/// A locale-aware number formatter.
#[derive(Debug, Clone)]
pub struct NumberFormatter {
    locale_tag: String,
    symbols: NumberSymbols,
    config: NumberFormat,
}

impl NumberFormatter {
    /// Create a formatter for the given locale tag using default decimal options.
    ///
    /// # Errors
    /// Returns `FormattingError::UnsupportedLocale` if the locale is not supported.
    pub fn for_locale(locale: &str) -> Result<Self, FormattingError> {
        Self::with_config(locale, NumberFormat::default())
    }

    /// Create a formatter with explicit configuration.
    ///
    /// # Errors
    /// Returns `FormattingError::UnsupportedLocale` if the locale is not supported.
    pub fn with_config(locale: &str, config: NumberFormat) -> Result<Self, FormattingError> {
        let data = lookup_locale_data(locale)
            .ok_or_else(|| FormattingError::UnsupportedLocale(locale.to_string()))?;
        Ok(Self::from_data(data, config))
    }

    /// Create from pinned locale data directly.
    #[must_use]
    pub fn from_data(data: &'static LocaleData, config: NumberFormat) -> Self {
        Self {
            locale_tag: data.tag.to_string(),
            symbols: data.number,
            config,
        }
    }

    /// Active locale tag.
    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale_tag
    }

    /// Format an integer value according to the locale's conventions.
    #[must_use]
    pub fn format_int<I: Into<i128>>(&self, value: I) -> String {
        let val = value.into();
        let is_negative = val < 0;
        let mut abs_val = val.unsigned_abs();
        if matches!(self.config.style, NumberStyle::Percent) {
            // `NumberStyle::Percent` is documented as "multiplied by 100", and
            // `format_float` does multiply. Without this the same formatter
            // rendered `12` as "12%" and `12.0` as "1,200%". Saturating rather
            // than wrapping: a value within a factor of 100 of `i128::MAX` has
            // no meaningful percentage, and this method cannot report an error.
            abs_val = abs_val.saturating_mul(100);
        }
        let raw_digits = abs_val.to_string();

        let padded_digits = if raw_digits.len() < self.config.min_integer_digits {
            format!(
                "{:0>width$}",
                raw_digits,
                width = self.config.min_integer_digits
            )
        } else {
            raw_digits
        };

        let grouped = if self.config.use_grouping {
            group_digits(
                &padded_digits,
                self.symbols.group_sep,
                self.symbols.group_size,
            )
        } else {
            padded_digits
        };

        let mut out = String::with_capacity(grouped.len() + 8);
        if is_negative {
            out.push_str(self.symbols.minus_sign);
        }

        match self.config.style {
            NumberStyle::Decimal => {
                out.push_str(&grouped);
                if self.config.min_fraction_digits > 0 {
                    out.push_str(self.symbols.decimal_sep);
                    for _ in 0..self.config.min_fraction_digits {
                        out.push('0');
                    }
                }
            }
            NumberStyle::Percent => match self.symbols.percent_placement {
                PercentPlacement::Prefix => {
                    out.push_str(self.symbols.percent_sign);
                    out.push_str(&grouped);
                }
                PercentPlacement::Suffix => {
                    out.push_str(&grouped);
                    out.push_str(self.symbols.percent_sign);
                }
                PercentPlacement::SuffixWithSpace => {
                    out.push_str(&grouped);
                    out.push(' ');
                    out.push_str(self.symbols.percent_sign);
                }
            },
            NumberStyle::Currency { symbol, .. } => {
                let sym = symbol.unwrap_or(self.symbols.default_currency_symbol);
                match self.symbols.currency_placement {
                    CurrencyPlacement::Prefix => {
                        out.push_str(sym);
                        out.push_str(&grouped);
                    }
                    CurrencyPlacement::PrefixWithSpace => {
                        out.push_str(sym);
                        out.push(' ');
                        out.push_str(&grouped);
                    }
                    CurrencyPlacement::Suffix => {
                        out.push_str(&grouped);
                        out.push_str(sym);
                    }
                    CurrencyPlacement::SuffixWithSpace => {
                        out.push_str(&grouped);
                        out.push(' ');
                        out.push_str(sym);
                    }
                }
            }
        }

        if self.config.numbering_system == NumberingSystem::Arab {
            convert_to_arab_digits(&out)
        } else {
            out
        }
    }

    /// Format a floating-point value according to the locale's conventions.
    ///
    /// # Errors
    /// Returns `FormattingError::NonFinite` if the value is NaN or infinite and
    /// `allow_non_finite` is false.
    pub fn format_float(&self, value: f64) -> Result<String, FormattingError> {
        if !value.is_finite() {
            if !self.config.allow_non_finite {
                return Err(FormattingError::NonFinite(value));
            }
            if value.is_nan() {
                return Ok("NaN".to_string());
            }
            return if value.is_sign_negative() {
                Ok(format!("{}∞", self.symbols.minus_sign))
            } else {
                Ok("∞".to_string())
            };
        }

        // Percent moves the decimal point two places rather than computing
        // `value * 100.0`, which rounds in binary (0.29 became
        // 28.999999999999996) and overflows to infinity near `f64::MAX`.
        let shift = if matches!(self.config.style, NumberStyle::Percent) {
            2
        } else {
            0
        };
        let is_negative = value.is_sign_negative() && value != 0.0;

        let (int_str, frac_str) = round_float_parts(
            value.abs(),
            shift,
            self.config.max_fraction_digits,
            self.config.min_fraction_digits,
            self.config.rounding_mode,
            is_negative,
        );

        let padded_int = if int_str.len() < self.config.min_integer_digits {
            format!(
                "{:0>width$}",
                int_str,
                width = self.config.min_integer_digits
            )
        } else {
            int_str
        };

        let grouped_int = if self.config.use_grouping {
            group_digits(&padded_int, self.symbols.group_sep, self.symbols.group_size)
        } else {
            padded_int
        };

        let mut num_str = String::with_capacity(grouped_int.len() + frac_str.len() + 4);
        num_str.push_str(&grouped_int);
        if !frac_str.is_empty() {
            num_str.push_str(self.symbols.decimal_sep);
            num_str.push_str(&frac_str);
        }

        let mut out = String::with_capacity(num_str.len() + 8);
        if is_negative {
            out.push_str(self.symbols.minus_sign);
        }

        match self.config.style {
            NumberStyle::Decimal => {
                out.push_str(&num_str);
            }
            NumberStyle::Percent => match self.symbols.percent_placement {
                PercentPlacement::Prefix => {
                    out.push_str(self.symbols.percent_sign);
                    out.push_str(&num_str);
                }
                PercentPlacement::Suffix => {
                    out.push_str(&num_str);
                    out.push_str(self.symbols.percent_sign);
                }
                PercentPlacement::SuffixWithSpace => {
                    out.push_str(&num_str);
                    out.push(' ');
                    out.push_str(self.symbols.percent_sign);
                }
            },
            NumberStyle::Currency { symbol, .. } => {
                let sym = symbol.unwrap_or(self.symbols.default_currency_symbol);
                match self.symbols.currency_placement {
                    CurrencyPlacement::Prefix => {
                        out.push_str(sym);
                        out.push_str(&num_str);
                    }
                    CurrencyPlacement::PrefixWithSpace => {
                        out.push_str(sym);
                        out.push(' ');
                        out.push_str(&num_str);
                    }
                    CurrencyPlacement::Suffix => {
                        out.push_str(&num_str);
                        out.push_str(sym);
                    }
                    CurrencyPlacement::SuffixWithSpace => {
                        out.push_str(&num_str);
                        out.push(' ');
                        out.push_str(sym);
                    }
                }
            }
        }

        if self.config.numbering_system == NumberingSystem::Arab {
            Ok(convert_to_arab_digits(&out))
        } else {
            Ok(out)
        }
    }
}

/// Helper to insert group separators into an integer string.
fn group_digits(digits: &str, group_sep: &str, group_size: usize) -> String {
    if group_size == 0 || digits.len() <= group_size {
        return digits.to_string();
    }

    let mut result =
        String::with_capacity(digits.len() + (digits.len() / group_size) * group_sep.len());
    let rem = digits.len() % group_size;

    if rem > 0 {
        result.push_str(&digits[..rem]);
        if rem < digits.len() {
            result.push_str(group_sep);
        }
    }

    for (i, chunk) in digits.as_bytes()[rem..].chunks(group_size).enumerate() {
        if i > 0 {
            result.push_str(group_sep);
        }
        result.push_str(std::str::from_utf8(chunk).unwrap_or(""));
    }

    result
}

/// Round a float's *magnitude* into integer digits and fraction digits.
///
/// Rounding works on the shortest decimal that round-trips to `val`, which is
/// what `Display` prints and what ICU rounds. It used to scale by
/// `10^max_frac` in `f64` and cast to `u128`. That saturated from about 3.4e38,
/// so `1e36` printed `u128::MAX` under the default three fraction digits and 40
/// fraction digits turned `0.5` into `1`. It also moved values that were not
/// ties: `(x + 0.5).floor()` took `0.49999999999999994` to 1 and `2^52 + 1` to
/// `2^52 + 2`, and a `1e-9` tie tolerance sent `2.5000000001` to 2 under
/// `HalfEven`. And it rounded the binary value rather than the decimal one the
/// caller wrote: `Ceil` gave `1.1` as `1.11` and `Truncate` gave `0.29` as
/// `0.28`.
///
/// `shift` moves the decimal point right before rounding, which is how percent
/// scales by 100 exactly. A `min_frac` above `max_frac` wins, as it does in
/// [`NumberFormatter::format_int`].
///
/// `val` is always non-negative: [`NumberFormatter::format_float`] splits the
/// sign off before calling and re-attaches it afterwards. That split is only
/// sound for the rounding modes that commute with `abs`. `HalfUp` (half away
/// from zero), `HalfEven` and `Truncate` (toward zero) are all defined in terms
/// of the magnitude already, so they survive it. `Floor` and `Ceil` are not:
/// they are defined against the number line, so below zero `Floor` rounds the
/// magnitude *up* and `Ceil` rounds it *down*. Rounding the bare magnitude
/// under those names gave `-2.9` as `-2` under `Floor` and `-2.1` as `-3` under
/// `Ceil` - each the opposite of what its own doc comment promises. Reflect
/// them here, which is the last point where the sign is still known.
fn round_float_parts(
    val: f64,
    shift: usize,
    max_frac: usize,
    min_frac: usize,
    mode: RoundingMode,
    is_negative: bool,
) -> (String, String) {
    let mode = if is_negative {
        match mode {
            // Toward -inf is away from zero; toward +inf is toward zero.
            RoundingMode::Floor => RoundingMode::Ceil,
            RoundingMode::Ceil => RoundingMode::Truncate,
            symmetric => symmetric,
        }
    } else {
        mode
    };

    // ASCII digits with the decimal point removed; `point` digits are integer.
    // `Display` never uses an exponent, so the integer part is never empty.
    let shortest = val.to_string();
    let (int_digits, frac_digits) = shortest.split_once('.').unwrap_or((&shortest, ""));
    let mut digits: Vec<u8> = int_digits.bytes().chain(frac_digits.bytes()).collect();
    let mut point = int_digits.len() + shift;
    if digits.len() < point {
        digits.resize(point, b'0');
    }

    let keep = point.saturating_add(max_frac.max(min_frac));
    let dropped = if digits.len() > keep {
        digits.split_off(keep)
    } else {
        Vec::new()
    };
    let round_up = match mode {
        RoundingMode::Truncate | RoundingMode::Floor => false,
        RoundingMode::Ceil => dropped.iter().any(|&d| d != b'0'),
        RoundingMode::HalfUp => dropped.first().is_some_and(|&d| d >= b'5'),
        RoundingMode::HalfEven => match dropped.split_first() {
            // An exact tie goes to the even neighbour.
            Some((&b'5', rest)) if rest.iter().all(|&d| d == b'0') => {
                digits.last().is_some_and(|&d| (d - b'0') % 2 == 1)
            }
            Some((&first, _)) => first >= b'5',
            None => false,
        },
    };
    if round_up {
        // Trailing 9s carry: they become 0s and the digit before them takes 1.
        if let Some(at) = digits.iter().rposition(|&d| d != b'9') {
            digits[at] += 1;
            digits[at + 1..].fill(b'0');
        } else {
            digits.fill(b'0');
            digits.insert(0, b'1');
            point += 1;
        }
    }

    let frac_digits = digits.split_off(point);
    let leading_zeros = digits
        .iter()
        .take_while(|&&d| d == b'0')
        .count()
        .min(digits.len() - 1);
    let int_str = digits[leading_zeros..]
        .iter()
        .map(|&d| char::from(d))
        .collect();

    let mut frac_str: String = frac_digits.iter().map(|&d| char::from(d)).collect();
    // Strip trailing zeros down to min_frac, then pad back up to it.
    while frac_str.len() > min_frac && frac_str.ends_with('0') {
        frac_str.pop();
    }
    frac_str.extend(std::iter::repeat_n(
        '0',
        min_frac.saturating_sub(frac_str.len()),
    ));

    (int_str, frac_str)
}

/// Convert ASCII digits in a string to Eastern Arabic numerals.
fn convert_to_arab_digits(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '0' => '٠',
            '1' => '١',
            '2' => '٢',
            '3' => '٣',
            '4' => '٤',
            '5' => '٥',
            '6' => '٦',
            '7' => '٧',
            '8' => '٨',
            '9' => '٩',
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_int_all_locales() {
        let val = 1_234_567;
        let en = NumberFormatter::for_locale("en").unwrap().format_int(val);
        assert_eq!(en, "1,234,567");

        let de = NumberFormatter::for_locale("de").unwrap().format_int(val);
        assert_eq!(de, "1.234.567");

        let fr = NumberFormatter::for_locale("fr").unwrap().format_int(val);
        assert_eq!(fr, "1\u{202f}234\u{202f}567");

        let es = NumberFormatter::for_locale("es").unwrap().format_int(val);
        assert_eq!(es, "1.234.567");

        let ru = NumberFormatter::for_locale("ru").unwrap().format_int(val);
        assert_eq!(ru, "1\u{00a0}234\u{00a0}567");

        let ar = NumberFormatter::for_locale("ar").unwrap().format_int(val);
        assert_eq!(ar, "1,234,567");

        let ja = NumberFormatter::for_locale("ja").unwrap().format_int(val);
        assert_eq!(ja, "1,234,567");
    }

    #[test]
    fn test_format_negative_int() {
        let fmt = NumberFormatter::for_locale("en").unwrap();
        assert_eq!(fmt.format_int(-42), "-42");
        assert_eq!(fmt.format_int(-1000), "-1,000");
    }

    #[test]
    fn test_format_float_decimal_separators() {
        let val = 1234.56;
        let cfg = NumberFormat::new().fraction_digits(2, 2);

        let en = NumberFormatter::with_config("en", cfg)
            .unwrap()
            .format_float(val)
            .unwrap();
        assert_eq!(en, "1,234.56");

        let de = NumberFormatter::with_config("de", cfg)
            .unwrap()
            .format_float(val)
            .unwrap();
        assert_eq!(de, "1.234,56");

        let fr = NumberFormatter::with_config("fr", cfg)
            .unwrap()
            .format_float(val)
            .unwrap();
        assert_eq!(fr, "1\u{202f}234,56");

        let es = NumberFormatter::with_config("es", cfg)
            .unwrap()
            .format_float(val)
            .unwrap();
        assert_eq!(es, "1.234,56");

        let ru = NumberFormatter::with_config("ru", cfg)
            .unwrap()
            .format_float(val)
            .unwrap();
        assert_eq!(ru, "1\u{00a0}234,56");

        let ar = NumberFormatter::with_config("ar", cfg)
            .unwrap()
            .format_float(val)
            .unwrap();
        assert_eq!(ar, "1,234.56");

        let ja = NumberFormatter::with_config("ja", cfg)
            .unwrap()
            .format_float(val)
            .unwrap();
        assert_eq!(ja, "1,234.56");
    }

    #[test]
    fn test_rounding_modes() {
        let half_up_cfg = NumberFormat::new()
            .fraction_digits(0, 0)
            .rounding_mode(RoundingMode::HalfUp);
        let half_even_cfg = NumberFormat::new()
            .fraction_digits(0, 0)
            .rounding_mode(RoundingMode::HalfEven);
        let trunc_cfg = NumberFormat::new()
            .fraction_digits(0, 0)
            .rounding_mode(RoundingMode::Truncate);
        let ceil_cfg = NumberFormat::new()
            .fraction_digits(0, 0)
            .rounding_mode(RoundingMode::Ceil);
        let floor_cfg = NumberFormat::new()
            .fraction_digits(0, 0)
            .rounding_mode(RoundingMode::Floor);

        let fmt_hu = NumberFormatter::with_config("en", half_up_cfg).unwrap();
        let fmt_he = NumberFormatter::with_config("en", half_even_cfg).unwrap();
        let fmt_tr = NumberFormatter::with_config("en", trunc_cfg).unwrap();
        let fmt_ce = NumberFormatter::with_config("en", ceil_cfg).unwrap();
        let fmt_fl = NumberFormatter::with_config("en", floor_cfg).unwrap();

        // HalfUp: 2.5 -> 3, 3.5 -> 4
        assert_eq!(fmt_hu.format_float(2.5).unwrap(), "3");
        assert_eq!(fmt_hu.format_float(3.5).unwrap(), "4");

        // HalfEven: 2.5 -> 2, 3.5 -> 4
        assert_eq!(fmt_he.format_float(2.5).unwrap(), "2");
        assert_eq!(fmt_he.format_float(3.5).unwrap(), "4");

        // Truncate / Floor / Ceil
        assert_eq!(fmt_tr.format_float(2.9).unwrap(), "2");
        assert_eq!(fmt_fl.format_float(2.9).unwrap(), "2");
        assert_eq!(fmt_ce.format_float(2.1).unwrap(), "3");
    }

    #[test]
    fn floor_and_ceil_follow_the_number_line_below_zero() {
        // `test_rounding_modes` above only ever feeds positive values, where
        // every mode agrees with its magnitude-only form. These are the cases
        // that separate them.
        let with = |mode| {
            NumberFormatter::with_config(
                "en",
                NumberFormat::new()
                    .fraction_digits(0, 0)
                    .rounding_mode(mode),
            )
            .unwrap()
        };
        let floor = with(RoundingMode::Floor);
        let ceil = with(RoundingMode::Ceil);
        let trunc = with(RoundingMode::Truncate);

        // Floor is toward -inf: it moves *away* from zero on a negative value.
        assert_eq!(floor.format_float(-2.1).unwrap(), "-3");
        assert_eq!(floor.format_float(-2.9).unwrap(), "-3");
        // Ceil is toward +inf: it moves *toward* zero on a negative value.
        assert_eq!(ceil.format_float(-2.1).unwrap(), "-2");
        assert_eq!(ceil.format_float(-2.9).unwrap(), "-2");
        // Truncate is toward zero from both sides - what Floor used to do.
        assert_eq!(trunc.format_float(-2.9).unwrap(), "-2");
        assert_eq!(trunc.format_float(2.9).unwrap(), "2");
        // Above zero the three are unchanged.
        assert_eq!(floor.format_float(2.9).unwrap(), "2");
        assert_eq!(ceil.format_float(2.1).unwrap(), "3");
    }

    #[test]
    fn floor_and_ceil_reflect_at_fractional_precision_too() {
        let with = |mode| {
            NumberFormatter::with_config(
                "en",
                NumberFormat::new()
                    .fraction_digits(2, 2)
                    .rounding_mode(mode),
            )
            .unwrap()
        };
        assert_eq!(
            with(RoundingMode::Floor).format_float(-1.234).unwrap(),
            "-1.24"
        );
        assert_eq!(
            with(RoundingMode::Ceil).format_float(-1.236).unwrap(),
            "-1.23"
        );
        assert_eq!(
            with(RoundingMode::Floor).format_float(1.236).unwrap(),
            "1.23"
        );
        assert_eq!(
            with(RoundingMode::Ceil).format_float(1.234).unwrap(),
            "1.24"
        );
    }

    fn format_with(min: usize, max: usize, mode: RoundingMode, value: f64) -> String {
        NumberFormatter::with_config(
            "en",
            NumberFormat::new()
                .fraction_digits(min, max)
                .rounding_mode(mode),
        )
        .unwrap()
        .format_float(value)
        .unwrap()
    }

    // Expected strings are what ICU prints (Node's `Intl.NumberFormat`).

    #[test]
    fn large_values_and_long_fractions_keep_their_digits() {
        // Scaling by 10^max_frac into a u128 saturated: both of these printed
        // u128::MAX's digits, and forty fraction digits turned 0.5 into 1.
        let default = NumberFormatter::for_locale("en").unwrap();
        assert_eq!(
            default.format_float(1e36).unwrap(),
            "1,000,000,000,000,000,000,000,000,000,000,000,000"
        );
        assert!(
            default
                .format_float(f64::MAX)
                .unwrap()
                .starts_with("179,769,313,486,231,570,000,")
        );
        assert_eq!(format_with(0, 40, RoundingMode::HalfUp, 0.5), "0.5");
    }

    #[test]
    fn rounding_reads_the_decimal_the_caller_wrote() {
        // Each of these came out one unit off when rounded in binary.
        assert_eq!(format_with(2, 2, RoundingMode::Ceil, 1.1), "1.10");
        assert_eq!(format_with(2, 2, RoundingMode::Truncate, 0.29), "0.29");
        assert_eq!(format_with(2, 2, RoundingMode::HalfUp, 1.005), "1.01");
        // `(x + 0.5).floor()` rounds these up although neither is a tie.
        assert_eq!(
            format_with(0, 0, RoundingMode::HalfUp, 0.499_999_999_999_999_94),
            "0"
        );
        assert_eq!(
            format_with(0, 0, RoundingMode::HalfUp, 4_503_599_627_370_497.0),
            "4,503,599,627,370,497"
        );
        // Only an exact tie goes to the even neighbour.
        assert_eq!(
            format_with(0, 0, RoundingMode::HalfEven, 2.500_000_000_1),
            "3"
        );
        assert_eq!(format_with(0, 0, RoundingMode::HalfEven, 2.5), "2");
        assert_eq!(format_with(1, 1, RoundingMode::HalfEven, 0.25), "0.2");
    }

    #[test]
    fn percent_moves_the_decimal_point_instead_of_multiplying() {
        let fmt = |mode| {
            NumberFormatter::with_config(
                "en",
                NumberFormat::new()
                    .style(NumberStyle::Percent)
                    .rounding_mode(mode),
            )
            .unwrap()
        };
        // `0.29 * 100.0` is 28.999999999999996, which truncated to "28.999%".
        assert_eq!(
            fmt(RoundingMode::Truncate).format_float(0.29).unwrap(),
            "29%"
        );
        // `1e307 * 100.0` overflowed to infinity.
        assert!(
            fmt(RoundingMode::HalfUp)
                .format_float(1e307)
                .unwrap()
                .starts_with("1,000,000,")
        );
    }

    #[test]
    fn a_minimum_above_the_maximum_fraction_digits_wins_for_floats_too() {
        let fmt =
            NumberFormatter::with_config("en", NumberFormat::new().fraction_digits(3, 1)).unwrap();
        assert_eq!(fmt.format_int(1), "1.000");
        assert_eq!(fmt.format_float(1.0).unwrap(), "1.000");
        assert_eq!(fmt.format_float(1.25).unwrap(), "1.250");
    }

    #[test]
    fn percent_style_scales_ints_and_floats_alike() {
        let fmt =
            NumberFormatter::with_config("en", NumberFormat::new().style(NumberStyle::Percent))
                .unwrap();

        // `NumberStyle::Percent` promises "multiplied by 100"; both entry
        // points have to keep that promise or one value renders two ways.
        assert_eq!(fmt.format_int(12), "1,200%");
        assert_eq!(fmt.format_float(12.0).unwrap(), "1,200%");
        assert_eq!(fmt.format_int(-1), "-100%");
        assert_eq!(fmt.format_int(0), "0%");

        // `i128::MAX * 100` overflows `u128`, and `format_int` has no error
        // channel, so the magnitude saturates instead of wrapping to a small
        // number.
        assert_eq!(
            fmt.format_int(i128::MAX),
            format!("{}%", group_digits(&u128::MAX.to_string(), ",", 3))
        );
    }

    #[test]
    fn test_currency_formatting() {
        let cfg = NumberFormat::new()
            .style(NumberStyle::Currency {
                code: None,
                symbol: None,
            })
            .fraction_digits(2, 2);

        let en = NumberFormatter::with_config("en", cfg)
            .unwrap()
            .format_float(1234.56)
            .unwrap();
        assert_eq!(en, "$1,234.56");

        let de = NumberFormatter::with_config("de", cfg)
            .unwrap()
            .format_float(1234.56)
            .unwrap();
        assert_eq!(de, "1.234,56 €");

        let ru = NumberFormatter::with_config("ru", cfg)
            .unwrap()
            .format_float(1234.56)
            .unwrap();
        assert_eq!(ru, "1\u{00a0}234,56 ₽");

        let ja = NumberFormatter::with_config("ja", cfg)
            .unwrap()
            .format_float(1234.56)
            .unwrap();
        assert_eq!(ja, "￥1,234.56");

        let ar = NumberFormatter::with_config("ar", cfg)
            .unwrap()
            .format_float(1234.56)
            .unwrap();
        assert_eq!(ar, "1,234.56 ر.س");
    }

    #[test]
    fn test_percent_formatting() {
        let cfg = NumberFormat::new()
            .style(NumberStyle::Percent)
            .fraction_digits(1, 1);

        let en = NumberFormatter::with_config("en", cfg)
            .unwrap()
            .format_float(0.125)
            .unwrap();
        assert_eq!(en, "12.5%");

        let de = NumberFormatter::with_config("de", cfg)
            .unwrap()
            .format_float(0.125)
            .unwrap();
        assert_eq!(de, "12,5 %");
    }

    #[test]
    fn test_eastern_arabic_digits() {
        let cfg = NumberFormat::new()
            .numbering_system(NumberingSystem::Arab)
            .fraction_digits(0, 0);
        let fmt = NumberFormatter::with_config("ar", cfg).unwrap();
        assert_eq!(fmt.format_int(12345), "١٢,٣٤٥");
    }

    #[test]
    fn test_non_finite_float_handling() {
        let fmt_strict = NumberFormatter::for_locale("en").unwrap();
        assert!(matches!(
            fmt_strict.format_float(f64::NAN),
            Err(FormattingError::NonFinite(_))
        ));
        assert!(matches!(
            fmt_strict.format_float(f64::INFINITY),
            Err(FormattingError::NonFinite(_))
        ));

        let cfg = NumberFormat::new().allow_non_finite(true);
        let fmt_permissive = NumberFormatter::with_config("en", cfg).unwrap();
        assert_eq!(fmt_permissive.format_float(f64::NAN).unwrap(), "NaN");
        assert_eq!(fmt_permissive.format_float(f64::INFINITY).unwrap(), "∞");
        assert_eq!(
            fmt_permissive.format_float(f64::NEG_INFINITY).unwrap(),
            "-∞"
        );
    }

    #[test]
    fn test_unsupported_locale() {
        let res = NumberFormatter::for_locale("xx-unsupported");
        assert!(matches!(res, Err(FormattingError::UnsupportedLocale(_))));
    }
}
