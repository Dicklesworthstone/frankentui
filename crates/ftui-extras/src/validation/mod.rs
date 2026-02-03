#![forbid(unsafe_code)]

//! Form validation framework with composable validators.
//!
//! This module provides a declarative validation system with:
//! - A core `Validator` trait for validating values
//! - Built-in validators for common patterns (required, min/max length, email, URL)
//! - Composable validators (And, Or, Not) for complex rules
//! - Error messages with parameter interpolation for i18n support
//! - Async validation deadline controller with survival analysis (bd-32x8)
//!
//! # Example
//!
//! ```rust
//! use ftui_extras::validation::{Validator, Required, MinLength, And, ValidationResult};
//!
//! // Simple validation
//! let required = Required::new();
//! assert!(required.validate("hello").is_valid());
//! assert!(!required.validate("").is_valid());
//!
//! // Composed validation
//! let username_validator = And::new(Required::new(), MinLength::new(3));
//! assert!(username_validator.validate("alice").is_valid());
//! assert!(!username_validator.validate("ab").is_valid());
//! ```
//!
//! Feature-gated under `validation`.

pub mod deadline;
mod validators;

pub use validators::{
    // Composition
    All,
    And,
    Any,
    // Error codes
    ERROR_CODE_EMAIL,
    ERROR_CODE_MAX_LENGTH,
    ERROR_CODE_MIN_LENGTH,
    ERROR_CODE_PATTERN,
    ERROR_CODE_RANGE,
    ERROR_CODE_REQUIRED,
    ERROR_CODE_URL,
    // Built-in validators
    Email,
    MaxLength,
    MinLength,
    Not,
    Or,
    Pattern,
    Range,
    Required,
    Url,
    // Core types
    ValidationError,
    ValidationResult,
    Validator,
    // Builder
    ValidatorBuilder,
};
