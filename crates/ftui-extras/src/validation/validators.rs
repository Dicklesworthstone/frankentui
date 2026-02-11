#![forbid(unsafe_code)]

//! Core validation types and built-in validators.

use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;

// ---------------------------------------------------------------------------
// Error Codes (for i18n lookup)
// ---------------------------------------------------------------------------

/// Error code for required field validation.
pub const ERROR_CODE_REQUIRED: &str = "required";
/// Error code for minimum length validation.
pub const ERROR_CODE_MIN_LENGTH: &str = "too_short";
/// Error code for maximum length validation.
pub const ERROR_CODE_MAX_LENGTH: &str = "too_long";
/// Error code for pattern validation.
pub const ERROR_CODE_PATTERN: &str = "pattern";
/// Error code for email validation.
pub const ERROR_CODE_EMAIL: &str = "email";
/// Error code for URL validation.
pub const ERROR_CODE_URL: &str = "url";
/// Error code for range validation.
pub const ERROR_CODE_RANGE: &str = "range";

// ---------------------------------------------------------------------------
// ValidationError
// ---------------------------------------------------------------------------

/// A validation error with code, message, and interpolation parameters.
///
/// The `code` field is a stable identifier for i18n systems.
/// The `message` field is a human-readable default message.
/// The `params` field contains key-value pairs for message interpolation.
///
/// # Example
///
/// ```rust
/// use ftui_extras::validation::ValidationError;
///
/// let error = ValidationError::new("too_short", "Must be at least {min} characters")
///     .with_param("min", 8);
///
/// assert_eq!(error.format_message(), "Must be at least 8 characters");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Stable error code for programmatic handling and i18n.
    pub code: &'static str,
    /// Human-readable error message template.
    pub message: String,
    /// Parameters for message interpolation.
    pub params: HashMap<String, String>,
}

impl ValidationError {
    /// Create a new validation error with the given code and message.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            params: HashMap::new(),
        }
    }

    /// Add a parameter for message interpolation.
    ///
    /// Parameters are substituted in the message using `{key}` syntax.
    #[must_use]
    pub fn with_param(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.params.insert(key.into(), value.to_string());
        self
    }

    /// Format the message with parameter substitution.
    ///
    /// Replaces `{key}` patterns in the message with corresponding parameter values.
    #[must_use]
    pub fn format_message(&self) -> String {
        let mut result = self.message.clone();
        for (key, value) in &self.params {
            result = result.replace(&format!("{{{key}}}"), value);
        }
        result
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format_message())
    }
}

impl std::error::Error for ValidationError {}

// ---------------------------------------------------------------------------
// ValidationResult
// ---------------------------------------------------------------------------

/// The result of a validation operation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ValidationResult {
    /// The value is valid.
    #[default]
    Valid,
    /// The value is invalid with an error.
    Invalid(ValidationError),
}

impl ValidationResult {
    /// Returns `true` if the result is `Valid`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }

    /// Returns `true` if the result is `Invalid`.
    #[must_use]
    pub fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid(_))
    }

    /// Returns the error if the result is `Invalid`, otherwise `None`.
    #[must_use]
    pub fn error(&self) -> Option<&ValidationError> {
        match self {
            Self::Valid => None,
            Self::Invalid(e) => Some(e),
        }
    }

    /// Returns the error message if the result is `Invalid`, otherwise `None`.
    #[must_use]
    pub fn error_message(&self) -> Option<String> {
        self.error().map(ValidationError::format_message)
    }

    /// Combine two results, returning the first error if any.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        match self {
            Self::Valid => other,
            Self::Invalid(_) => self,
        }
    }

    /// Combine two results, returning `Valid` if either is valid.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        match self {
            Self::Valid => Self::Valid,
            Self::Invalid(_) => other,
        }
    }
}

// ---------------------------------------------------------------------------
// Validator Trait
// ---------------------------------------------------------------------------

/// A trait for validating values of type `T`.
///
/// Validators are composable and can be combined using `And`, `Or`, and `Not`.
///
/// # Implementing a Custom Validator
///
/// ```rust
/// use ftui_extras::validation::{Validator, ValidationResult, ValidationError};
///
/// struct NoSpaces;
///
/// impl Validator<str> for NoSpaces {
///     fn validate(&self, value: &str) -> ValidationResult {
///         if value.contains(' ') {
///             ValidationResult::Invalid(
///                 ValidationError::new("no_spaces", "Value must not contain spaces")
///             )
///         } else {
///             ValidationResult::Valid
///         }
///     }
///
///     fn error_message(&self) -> &str {
///         "Value must not contain spaces"
///     }
/// }
/// ```
pub trait Validator<T: ?Sized>: Send + Sync {
    /// Validate the given value.
    fn validate(&self, value: &T) -> ValidationResult;

    /// Return the default error message for this validator.
    fn error_message(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Built-in Validators
// ---------------------------------------------------------------------------

/// Validates that a string is not empty.
///
/// By default, whitespace-only strings are considered empty.
#[derive(Debug, Clone, Copy, Default)]
pub struct Required {
    /// If `true`, whitespace-only strings are considered valid.
    pub allow_whitespace: bool,
}

impl Required {
    /// Create a new `Required` validator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow whitespace-only strings to pass validation.
    #[must_use]
    pub fn allow_whitespace(mut self) -> Self {
        self.allow_whitespace = true;
        self
    }
}

impl Validator<str> for Required {
    fn validate(&self, value: &str) -> ValidationResult {
        let is_empty = if self.allow_whitespace {
            value.is_empty()
        } else {
            value.trim().is_empty()
        };

        if is_empty {
            ValidationResult::Invalid(ValidationError::new(
                ERROR_CODE_REQUIRED,
                "This field is required",
            ))
        } else {
            ValidationResult::Valid
        }
    }

    fn error_message(&self) -> &str {
        "This field is required"
    }
}

/// Validates that a string has at least `min` characters.
#[derive(Debug, Clone, Copy)]
pub struct MinLength {
    /// Minimum number of characters required.
    pub min: usize,
}

impl MinLength {
    /// Create a new `MinLength` validator.
    #[must_use]
    pub fn new(min: usize) -> Self {
        Self { min }
    }
}

impl Validator<str> for MinLength {
    fn validate(&self, value: &str) -> ValidationResult {
        let len = value.chars().count();
        if len < self.min {
            ValidationResult::Invalid(
                ValidationError::new(ERROR_CODE_MIN_LENGTH, "Must be at least {min} characters")
                    .with_param("min", self.min)
                    .with_param("actual", len),
            )
        } else {
            ValidationResult::Valid
        }
    }

    fn error_message(&self) -> &str {
        "Must be at least {min} characters"
    }
}

/// Validates that a string has at most `max` characters.
#[derive(Debug, Clone, Copy)]
pub struct MaxLength {
    /// Maximum number of characters allowed.
    pub max: usize,
}

impl MaxLength {
    /// Create a new `MaxLength` validator.
    #[must_use]
    pub fn new(max: usize) -> Self {
        Self { max }
    }
}

impl Validator<str> for MaxLength {
    fn validate(&self, value: &str) -> ValidationResult {
        let len = value.chars().count();
        if len > self.max {
            ValidationResult::Invalid(
                ValidationError::new(ERROR_CODE_MAX_LENGTH, "Must be at most {max} characters")
                    .with_param("max", self.max)
                    .with_param("actual", len),
            )
        } else {
            ValidationResult::Valid
        }
    }

    fn error_message(&self) -> &str {
        "Must be at most {max} characters"
    }
}

/// Validates that a string matches a regular expression pattern.
///
/// Note: This validator uses simple pattern matching without a regex engine.
/// For complex patterns, consider implementing a custom validator with the `regex` crate.
#[derive(Debug, Clone)]
pub struct Pattern {
    /// The pattern to match (simple contains check).
    pub pattern: String,
    /// Custom error message.
    pub message: String,
    /// Whether to match the whole string (starts and ends with).
    pub exact: bool,
}

impl Pattern {
    /// Create a new `Pattern` validator that checks if the value contains the pattern.
    #[must_use]
    pub fn contains(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            message: "Invalid format".to_string(),
            exact: false,
        }
    }

    /// Create a new `Pattern` validator that checks if the value equals the pattern.
    #[must_use]
    pub fn exact(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            message: "Invalid format".to_string(),
            exact: true,
        }
    }

    /// Set a custom error message.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }
}

impl Validator<str> for Pattern {
    fn validate(&self, value: &str) -> ValidationResult {
        let matches = if self.exact {
            value == self.pattern
        } else {
            value.contains(&self.pattern)
        };

        if matches {
            ValidationResult::Valid
        } else {
            ValidationResult::Invalid(ValidationError::new(ERROR_CODE_PATTERN, &self.message))
        }
    }

    fn error_message(&self) -> &str {
        &self.message
    }
}

/// Validates that a string is a valid email address.
///
/// Uses a simple heuristic check: contains `@` with text before and after.
#[derive(Debug, Clone, Copy, Default)]
pub struct Email;

impl Email {
    /// Create a new `Email` validator.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Validator<str> for Email {
    fn validate(&self, value: &str) -> ValidationResult {
        // Simple email validation: contains @ with text on both sides
        let trimmed = value.trim();

        if trimmed.is_empty() {
            return ValidationResult::Valid; // Empty is valid (use Required for required)
        }

        let parts: Vec<&str> = trimmed.splitn(2, '@').collect();
        if parts.len() != 2 {
            return ValidationResult::Invalid(ValidationError::new(
                ERROR_CODE_EMAIL,
                "Invalid email address",
            ));
        }

        let (local, domain) = (parts[0], parts[1]);

        // Basic checks
        if local.is_empty() || domain.is_empty() {
            return ValidationResult::Invalid(ValidationError::new(
                ERROR_CODE_EMAIL,
                "Invalid email address",
            ));
        }

        // Domain must contain at least one dot (simple heuristic)
        if !domain.contains('.') {
            return ValidationResult::Invalid(ValidationError::new(
                ERROR_CODE_EMAIL,
                "Invalid email address",
            ));
        }

        // Domain parts after split by '.' must be non-empty
        let domain_parts: Vec<&str> = domain.split('.').collect();
        if domain_parts.iter().any(|p| p.is_empty()) {
            return ValidationResult::Invalid(ValidationError::new(
                ERROR_CODE_EMAIL,
                "Invalid email address",
            ));
        }

        // TLD must be at least 2 characters
        if let Some(tld) = domain_parts.last()
            && tld.len() < 2
        {
            return ValidationResult::Invalid(ValidationError::new(
                ERROR_CODE_EMAIL,
                "Invalid email address",
            ));
        }

        ValidationResult::Valid
    }

    fn error_message(&self) -> &str {
        "Invalid email address"
    }
}

/// Validates that a string is a valid URL.
///
/// Uses a simple heuristic check: starts with `http://` or `https://`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Url {
    /// If `true`, require HTTPS only.
    pub require_https: bool,
}

impl Url {
    /// Create a new `Url` validator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Require HTTPS URLs only.
    #[must_use]
    pub fn require_https(mut self) -> Self {
        self.require_https = true;
        self
    }
}

impl Validator<str> for Url {
    fn validate(&self, value: &str) -> ValidationResult {
        let trimmed = value.trim();

        if trimmed.is_empty() {
            return ValidationResult::Valid; // Empty is valid (use Required for required)
        }

        let is_valid = if self.require_https {
            trimmed.starts_with("https://") && trimmed.len() > 8
        } else {
            (trimmed.starts_with("http://") && trimmed.len() > 7)
                || (trimmed.starts_with("https://") && trimmed.len() > 8)
        };

        if is_valid {
            ValidationResult::Valid
        } else {
            let message = if self.require_https {
                "Invalid URL (must use HTTPS)"
            } else {
                "Invalid URL"
            };
            ValidationResult::Invalid(ValidationError::new(ERROR_CODE_URL, message))
        }
    }

    fn error_message(&self) -> &str {
        "Invalid URL"
    }
}

/// Validates that a value is within a range.
#[derive(Debug, Clone, Copy)]
pub struct Range<T> {
    /// Minimum value (inclusive).
    pub min: T,
    /// Maximum value (inclusive).
    pub max: T,
}

impl<T: Copy> Range<T> {
    /// Create a new `Range` validator.
    #[must_use]
    pub fn new(min: T, max: T) -> Self {
        Self { min, max }
    }
}

impl<T> Validator<T> for Range<T>
where
    T: PartialOrd + fmt::Display + Copy + Send + Sync,
{
    fn validate(&self, value: &T) -> ValidationResult {
        if *value >= self.min && *value <= self.max {
            ValidationResult::Valid
        } else {
            ValidationResult::Invalid(
                ValidationError::new(ERROR_CODE_RANGE, "Must be between {min} and {max}")
                    .with_param("min", self.min)
                    .with_param("max", self.max)
                    .with_param("actual", *value),
            )
        }
    }

    fn error_message(&self) -> &str {
        "Must be between {min} and {max}"
    }
}

// ---------------------------------------------------------------------------
// Composition Validators
// ---------------------------------------------------------------------------

/// Combines two validators with AND logic.
///
/// Both validators must pass for the result to be valid.
#[derive(Debug, Clone)]
pub struct And<A, B> {
    /// First validator.
    pub first: A,
    /// Second validator.
    pub second: B,
}

impl<A, B> And<A, B> {
    /// Create a new `And` validator.
    #[must_use]
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<T: ?Sized, A, B> Validator<T> for And<A, B>
where
    A: Validator<T>,
    B: Validator<T>,
{
    fn validate(&self, value: &T) -> ValidationResult {
        match self.first.validate(value) {
            ValidationResult::Valid => self.second.validate(value),
            err => err,
        }
    }

    fn error_message(&self) -> &str {
        self.first.error_message()
    }
}

/// Combines two validators with OR logic.
///
/// At least one validator must pass for the result to be valid.
#[derive(Debug, Clone)]
pub struct Or<A, B> {
    /// First validator.
    pub first: A,
    /// Second validator.
    pub second: B,
}

impl<A, B> Or<A, B> {
    /// Create a new `Or` validator.
    #[must_use]
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<T: ?Sized, A, B> Validator<T> for Or<A, B>
where
    A: Validator<T>,
    B: Validator<T>,
{
    fn validate(&self, value: &T) -> ValidationResult {
        match self.first.validate(value) {
            ValidationResult::Valid => ValidationResult::Valid,
            _ => self.second.validate(value),
        }
    }

    fn error_message(&self) -> &str {
        self.second.error_message()
    }
}

/// Negates a validator.
///
/// The result is valid if the inner validator fails, and vice versa.
#[derive(Debug, Clone)]
pub struct Not<V> {
    /// Inner validator.
    pub inner: V,
    /// Error message when the inner validator passes (and this should fail).
    pub message: String,
}

impl<V> Not<V> {
    /// Create a new `Not` validator with a custom error message.
    #[must_use]
    pub fn new(inner: V, message: impl Into<String>) -> Self {
        Self {
            inner,
            message: message.into(),
        }
    }
}

impl<T: ?Sized, V> Validator<T> for Not<V>
where
    V: Validator<T>,
{
    fn validate(&self, value: &T) -> ValidationResult {
        match self.inner.validate(value) {
            ValidationResult::Valid => {
                ValidationResult::Invalid(ValidationError::new("not", &self.message))
            }
            ValidationResult::Invalid(_) => ValidationResult::Valid,
        }
    }

    fn error_message(&self) -> &str {
        &self.message
    }
}

/// Combines multiple validators with AND logic.
///
/// All validators must pass for the result to be valid.
pub struct All<T: ?Sized> {
    validators: Vec<Box<dyn Validator<T>>>,
}

impl<T: ?Sized> All<T> {
    /// Create a new `All` validator with the given validators.
    #[must_use]
    pub fn new(validators: Vec<Box<dyn Validator<T>>>) -> Self {
        Self { validators }
    }
}

impl<T: ?Sized> Validator<T> for All<T> {
    fn validate(&self, value: &T) -> ValidationResult {
        for validator in &self.validators {
            let result = validator.validate(value);
            if result.is_invalid() {
                return result;
            }
        }
        ValidationResult::Valid
    }

    fn error_message(&self) -> &str {
        self.validators
            .first()
            .map_or("Validation failed", |v| v.error_message())
    }
}

impl<T: ?Sized> fmt::Debug for All<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("All")
            .field(
                "validators",
                &format!("[{} validators]", self.validators.len()),
            )
            .finish()
    }
}

/// Combines multiple validators with OR logic.
///
/// At least one validator must pass for the result to be valid.
pub struct Any<T: ?Sized> {
    validators: Vec<Box<dyn Validator<T>>>,
}

impl<T: ?Sized> Any<T> {
    /// Create a new `Any` validator with the given validators.
    #[must_use]
    pub fn new(validators: Vec<Box<dyn Validator<T>>>) -> Self {
        Self { validators }
    }
}

impl<T: ?Sized> Validator<T> for Any<T> {
    fn validate(&self, value: &T) -> ValidationResult {
        let mut last_error = None;
        for validator in &self.validators {
            let result = validator.validate(value);
            if result.is_valid() {
                return ValidationResult::Valid;
            }
            last_error = result.error().cloned();
        }
        last_error.map_or(ValidationResult::Valid, ValidationResult::Invalid)
    }

    fn error_message(&self) -> &str {
        self.validators
            .last()
            .map_or("Validation failed", |v| v.error_message())
    }
}

impl<T: ?Sized> fmt::Debug for Any<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Any")
            .field(
                "validators",
                &format!("[{} validators]", self.validators.len()),
            )
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ValidatorBuilder
// ---------------------------------------------------------------------------

/// A builder for constructing validators fluently.
///
/// # Example
///
/// ```rust
/// use ftui_extras::validation::{ValidatorBuilder, Validator};
///
/// let validator = ValidatorBuilder::<str>::new()
///     .required()
///     .min_length(3)
///     .max_length(20)
///     .build();
///
/// assert!(validator.validate("alice").is_valid());
/// assert!(!validator.validate("ab").is_valid());
/// ```
#[must_use]
pub struct ValidatorBuilder<T: ?Sized> {
    validators: Vec<Box<dyn Validator<T>>>,
    _phantom: PhantomData<T>,
}

impl<T: ?Sized> Default for ValidatorBuilder<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ?Sized> ValidatorBuilder<T> {
    /// Create a new empty validator builder.
    pub fn new() -> Self {
        Self {
            validators: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Add a custom validator.
    pub fn custom(mut self, validator: impl Validator<T> + 'static) -> Self {
        self.validators.push(Box::new(validator));
        self
    }

    /// Build the combined validator.
    #[must_use]
    pub fn build(self) -> All<T> {
        All::new(self.validators)
    }
}

impl ValidatorBuilder<str> {
    /// Add a `Required` validator.
    pub fn required(self) -> Self {
        self.custom(Required::new())
    }

    /// Add a `MinLength` validator.
    pub fn min_length(self, min: usize) -> Self {
        self.custom(MinLength::new(min))
    }

    /// Add a `MaxLength` validator.
    pub fn max_length(self, max: usize) -> Self {
        self.custom(MaxLength::new(max))
    }

    /// Add an `Email` validator.
    pub fn email(self) -> Self {
        self.custom(Email::new())
    }

    /// Add a `Url` validator.
    pub fn url(self) -> Self {
        self.custom(Url::new())
    }

    /// Add a pattern contains check.
    pub fn contains(self, pattern: impl Into<String>) -> Self {
        self.custom(Pattern::contains(pattern))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ValidationError tests --

    #[test]
    fn validation_error_new() {
        let err = ValidationError::new("test", "Test message");
        assert_eq!(err.code, "test");
        assert_eq!(err.message, "Test message");
        assert!(err.params.is_empty());
    }

    #[test]
    fn validation_error_with_param() {
        let err = ValidationError::new("test", "Value is {value}").with_param("value", 42);
        assert_eq!(err.params.get("value"), Some(&"42".to_string()));
    }

    #[test]
    fn validation_error_format_message() {
        let err =
            ValidationError::new("test", "Must be at least {min} characters").with_param("min", 8);
        assert_eq!(err.format_message(), "Must be at least 8 characters");
    }

    #[test]
    fn validation_error_format_multiple_params() {
        let err = ValidationError::new("test", "Between {min} and {max}")
            .with_param("min", 1)
            .with_param("max", 10);
        assert_eq!(err.format_message(), "Between 1 and 10");
    }

    #[test]
    fn validation_error_display() {
        let err = ValidationError::new("test", "Error: {code}").with_param("code", "E001");
        assert_eq!(format!("{err}"), "Error: E001");
    }

    // -- ValidationResult tests --

    #[test]
    fn validation_result_is_valid() {
        assert!(ValidationResult::Valid.is_valid());
        assert!(!ValidationResult::Invalid(ValidationError::new("", "")).is_valid());
    }

    #[test]
    fn validation_result_is_invalid() {
        assert!(!ValidationResult::Valid.is_invalid());
        assert!(ValidationResult::Invalid(ValidationError::new("", "")).is_invalid());
    }

    #[test]
    fn validation_result_error() {
        let valid = ValidationResult::Valid;
        assert!(valid.error().is_none());

        let invalid = ValidationResult::Invalid(ValidationError::new("test", "msg"));
        assert!(invalid.error().is_some());
        assert_eq!(invalid.error().unwrap().code, "test");
    }

    #[test]
    fn validation_result_and() {
        let valid = ValidationResult::Valid;
        let invalid = ValidationResult::Invalid(ValidationError::new("", ""));

        assert!(valid.clone().and(valid.clone()).is_valid());
        assert!(valid.clone().and(invalid.clone()).is_invalid());
        assert!(invalid.clone().and(valid.clone()).is_invalid());
        assert!(invalid.clone().and(invalid.clone()).is_invalid());
    }

    #[test]
    fn validation_result_or() {
        let valid = ValidationResult::Valid;
        let invalid = ValidationResult::Invalid(ValidationError::new("", ""));

        assert!(valid.clone().or(valid.clone()).is_valid());
        assert!(valid.clone().or(invalid.clone()).is_valid());
        assert!(invalid.clone().or(valid.clone()).is_valid());
        assert!(invalid.clone().or(invalid.clone()).is_invalid());
    }

    // -- Required tests --

    #[test]
    fn required_empty_fails() {
        let v = Required::new();
        assert!(v.validate("").is_invalid());
    }

    #[test]
    fn required_whitespace_only_fails() {
        let v = Required::new();
        assert!(v.validate("   ").is_invalid());
        assert!(v.validate("\t\n").is_invalid());
    }

    #[test]
    fn required_whitespace_allowed() {
        let v = Required::new().allow_whitespace();
        assert!(v.validate("   ").is_valid());
    }

    #[test]
    fn required_non_empty_passes() {
        let v = Required::new();
        assert!(v.validate("hello").is_valid());
        assert!(v.validate("  hello  ").is_valid());
    }

    // -- MinLength tests --

    #[test]
    fn min_length_boundary() {
        let v = MinLength::new(3);
        assert!(v.validate("ab").is_invalid()); // 2 < 3
        assert!(v.validate("abc").is_valid()); // 3 == 3
        assert!(v.validate("abcd").is_valid()); // 4 > 3
    }

    #[test]
    fn min_length_unicode() {
        let v = MinLength::new(4);
        assert!(v.validate("café").is_valid()); // 4 characters
        assert!(v.validate("caf").is_invalid()); // 3 characters
    }

    #[test]
    fn min_length_emoji() {
        let v = MinLength::new(1);
        assert!(v.validate("🎉").is_valid()); // 1 character (emoji)
    }

    #[test]
    fn min_length_error_params() {
        let v = MinLength::new(5);
        let result = v.validate("ab");
        match result {
            ValidationResult::Invalid(err) => {
                assert_eq!(err.params.get("min"), Some(&"5".to_string()));
                assert_eq!(err.params.get("actual"), Some(&"2".to_string()));
            }
            other => {
                assert!(other.is_invalid(), "Expected invalid result");
            }
        }
    }

    // -- MaxLength tests --

    #[test]
    fn max_length_boundary() {
        let v = MaxLength::new(3);
        assert!(v.validate("ab").is_valid()); // 2 < 3
        assert!(v.validate("abc").is_valid()); // 3 == 3
        assert!(v.validate("abcd").is_invalid()); // 4 > 3
    }

    #[test]
    fn max_length_unicode() {
        let v = MaxLength::new(4);
        assert!(v.validate("café").is_valid()); // 4 characters
        assert!(v.validate("café!").is_invalid()); // 5 characters
    }

    // -- Pattern tests --

    #[test]
    fn pattern_contains() {
        let v = Pattern::contains("@");
        assert!(v.validate("test@example").is_valid());
        assert!(v.validate("no at sign").is_invalid());
    }

    #[test]
    fn pattern_exact() {
        let v = Pattern::exact("hello");
        assert!(v.validate("hello").is_valid());
        assert!(v.validate("hello!").is_invalid());
        assert!(v.validate("HELLO").is_invalid());
    }

    #[test]
    fn pattern_custom_message() {
        let v = Pattern::contains("@").with_message("Must contain @");
        let result = v.validate("test");
        if let ValidationResult::Invalid(err) = result {
            assert_eq!(err.message, "Must contain @");
        }
    }

    // -- Email tests --

    #[test]
    fn email_valid() {
        let v = Email::new();
        assert!(v.validate("user@example.com").is_valid());
        assert!(v.validate("user.name@example.co.uk").is_valid());
        assert!(v.validate("user+tag@example.org").is_valid());
    }

    #[test]
    fn email_invalid() {
        let v = Email::new();
        assert!(v.validate("not-an-email").is_invalid());
        assert!(v.validate("@example.com").is_invalid());
        assert!(v.validate("user@").is_invalid());
        assert!(v.validate("user@example").is_invalid()); // No TLD
        assert!(v.validate("user@.com").is_invalid());
    }

    #[test]
    fn email_empty_is_valid() {
        let v = Email::new();
        assert!(v.validate("").is_valid()); // Use Required for required
    }

    #[test]
    fn email_with_whitespace() {
        let v = Email::new();
        assert!(v.validate("  user@example.com  ").is_valid()); // Trimmed
    }

    // -- Url tests --

    #[test]
    fn url_valid() {
        let v = Url::new();
        assert!(v.validate("http://example.com").is_valid());
        assert!(v.validate("https://example.com").is_valid());
        assert!(v.validate("https://example.com/path?query=1").is_valid());
    }

    #[test]
    fn url_invalid() {
        let v = Url::new();
        assert!(v.validate("not-a-url").is_invalid());
        assert!(v.validate("ftp://example.com").is_invalid());
        assert!(v.validate("http://").is_invalid());
    }

    #[test]
    fn url_require_https() {
        let v = Url::new().require_https();
        assert!(v.validate("https://example.com").is_valid());
        assert!(v.validate("http://example.com").is_invalid());
    }

    #[test]
    fn url_empty_is_valid() {
        let v = Url::new();
        assert!(v.validate("").is_valid()); // Use Required for required
    }

    // -- Range tests --

    #[test]
    fn range_i32() {
        let v = Range::new(1, 10);
        assert!(v.validate(&0).is_invalid());
        assert!(v.validate(&1).is_valid());
        assert!(v.validate(&5).is_valid());
        assert!(v.validate(&10).is_valid());
        assert!(v.validate(&11).is_invalid());
    }

    #[test]
    fn range_f64() {
        let v = Range::new(0.0, 1.0);
        assert!(v.validate(&0.5).is_valid());
        assert!(v.validate(&1.5).is_invalid());
    }

    // -- And tests --

    #[test]
    fn and_both_valid() {
        let v = And::new(Required::new(), MinLength::new(3));
        assert!(v.validate("hello").is_valid());
    }

    #[test]
    fn and_first_invalid() {
        let v = And::new(Required::new(), MinLength::new(3));
        assert!(v.validate("").is_invalid());
        // Error should be from Required
        if let ValidationResult::Invalid(err) = v.validate("") {
            assert_eq!(err.code, ERROR_CODE_REQUIRED);
        }
    }

    #[test]
    fn and_second_invalid() {
        let v = And::new(Required::new(), MinLength::new(5));
        let result = v.validate("ab");
        assert!(result.is_invalid());
        // Error should be from MinLength
        if let ValidationResult::Invalid(err) = result {
            assert_eq!(err.code, ERROR_CODE_MIN_LENGTH);
        }
    }

    // -- Or tests --

    #[test]
    fn or_first_valid() {
        let v = Or::new(Pattern::exact("yes"), Pattern::exact("no"));
        assert!(v.validate("yes").is_valid());
    }

    #[test]
    fn or_second_valid() {
        let v = Or::new(Pattern::exact("yes"), Pattern::exact("no"));
        assert!(v.validate("no").is_valid());
    }

    #[test]
    fn or_neither_valid() {
        let v = Or::new(Pattern::exact("yes"), Pattern::exact("no"));
        assert!(v.validate("maybe").is_invalid());
    }

    // -- Not tests --

    #[test]
    fn not_inverts_valid() {
        let v = Not::new(Pattern::contains("@"), "Must not contain @");
        assert!(v.validate("hello").is_valid());
        assert!(v.validate("hello@world").is_invalid());
    }

    // -- All tests --

    #[test]
    fn all_validators() {
        let v: All<str> = All::new(vec![
            Box::new(Required::new()),
            Box::new(MinLength::new(3)),
            Box::new(MaxLength::new(10)),
        ]);
        assert!(v.validate("hello").is_valid());
        assert!(v.validate("").is_invalid());
        assert!(v.validate("ab").is_invalid());
        assert!(v.validate("this is too long").is_invalid());
    }

    // -- Any tests --

    #[test]
    fn any_validators() {
        let v: Any<str> = Any::new(vec![
            Box::new(Pattern::exact("yes")),
            Box::new(Pattern::exact("no")),
            Box::new(Pattern::exact("maybe")),
        ]);
        assert!(v.validate("yes").is_valid());
        assert!(v.validate("no").is_valid());
        assert!(v.validate("maybe").is_valid());
        assert!(v.validate("dunno").is_invalid());
    }

    // -- ValidatorBuilder tests --

    #[test]
    fn builder_empty() {
        let v = ValidatorBuilder::<str>::new().build();
        assert!(v.validate("anything").is_valid());
    }

    #[test]
    fn builder_required() {
        let v = ValidatorBuilder::<str>::new().required().build();
        assert!(v.validate("hello").is_valid());
        assert!(v.validate("").is_invalid());
    }

    #[test]
    fn builder_chain() {
        let v = ValidatorBuilder::<str>::new()
            .required()
            .min_length(3)
            .max_length(10)
            .build();

        assert!(v.validate("hello").is_valid());
        assert!(v.validate("").is_invalid());
        assert!(v.validate("ab").is_invalid());
        assert!(v.validate("this is way too long").is_invalid());
    }

    #[test]
    fn builder_email() {
        let v = ValidatorBuilder::<str>::new().required().email().build();

        assert!(v.validate("user@example.com").is_valid());
        assert!(v.validate("").is_invalid());
        assert!(v.validate("not-an-email").is_invalid());
    }

    #[test]
    fn builder_url() {
        let v = ValidatorBuilder::<str>::new().required().url().build();

        assert!(v.validate("https://example.com").is_valid());
        assert!(v.validate("").is_invalid());
        assert!(v.validate("not-a-url").is_invalid());
    }

    // -- Custom validator test --

    struct NoDigits;

    impl Validator<str> for NoDigits {
        fn validate(&self, value: &str) -> ValidationResult {
            if value.chars().any(|c| c.is_ascii_digit()) {
                ValidationResult::Invalid(ValidationError::new(
                    "no_digits",
                    "Must not contain digits",
                ))
            } else {
                ValidationResult::Valid
            }
        }

        fn error_message(&self) -> &str {
            "Must not contain digits"
        }
    }

    #[test]
    fn custom_validator() {
        let v = ValidatorBuilder::<str>::new()
            .required()
            .custom(NoDigits)
            .build();

        assert!(v.validate("hello").is_valid());
        assert!(v.validate("hello123").is_invalid());
    }

    // -- Edge cases --

    #[test]
    fn empty_string_with_min_length() {
        let v = MinLength::new(0);
        assert!(v.validate("").is_valid());
    }

    #[test]
    fn zero_max_length() {
        let v = MaxLength::new(0);
        assert!(v.validate("").is_valid());
        assert!(v.validate("a").is_invalid());
    }

    #[test]
    fn validation_error_equality() {
        let err1 = ValidationError::new("test", "Message");
        let err2 = ValidationError::new("test", "Message");
        assert_eq!(err1, err2);

        let err3 = ValidationError::new("test", "Different");
        assert_ne!(err1, err3);
    }
}
