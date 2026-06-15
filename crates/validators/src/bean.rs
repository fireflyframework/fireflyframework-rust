// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Declarative bean validation — the JSR-380 (`jakarta.validation`)
//! analog for the Firefly Framework.
//!
//! Where the crate root ships standalone predicate functions
//! ([`validate_email`](crate::validate_email), …), this module supplies
//! the *constraint set* a struct opts into through the
//! `#[derive(Validate)]` macro: the [`Validate`] trait, a per-field
//! [`ValidationError`], the [`ValidationErrors`] collection that gathers
//! every failing constraint, and the conversion onto the framework's
//! canonical [`FireflyError`] (a 422 `application/problem+json`).
//!
//! The derive macro lives in `firefly-macros`; it reads `#[validate(...)]`
//! attributes off each field and emits an [`impl Validate`](Validate)
//! whose body pushes a [`ValidationError`] per violated constraint. This
//! mirrors Spring's `@Valid` bean validation and pyfly's pydantic-style
//! field validators, collapsed onto the Rust trait below.

use std::fmt;

use firefly_kernel::{FireflyError, TYPE_VALIDATION};
use serde_json::{json, Value};

/// Runtime items `#[derive(Validate)]`-generated code resolves through the
/// facade, so a service that lists only `firefly` (and never `regex`) still
/// compiles a `#[validate(pattern = "...")]` check.
///
/// You should never name this module by hand; it exists solely for the
/// macro contract (the analog of `firefly`'s `__rt`).
#[doc(hidden)]
pub mod __rt {
    pub use regex::Regex;
    pub use std::sync::LazyLock;
}

/// A single field-level constraint violation — the analog of a JSR-380
/// `ConstraintViolation`.
///
/// `field` names the offending field, `code` is the stable machine
/// identifier of the violated constraint (e.g. `"email"`, `"length"`,
/// `"range"`), and `message` is the human-readable reason. The `code`
/// values match the `#[validate(...)]` constraint keywords so a client
/// can dispatch on them across the Java/.NET/Go/Python ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// The name of the field that failed (the Rust field identifier).
    pub field: String,
    /// The stable constraint code, e.g. `"not_empty"` or `"length"`.
    pub code: &'static str,
    /// The human-readable failure reason.
    pub message: String,
}

impl ValidationError {
    /// Builds a violation for `field` from a constraint `code` and a
    /// human-readable `message`.
    pub fn new(field: impl Into<String>, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.field, self.message, self.code)
    }
}

/// The accumulated set of constraint violations produced by a single
/// [`Validate::validate`] call — the analog of a JSR-380
/// `ConstraintViolationException`'s violation set.
///
/// A `Validate` impl runs *every* constraint and collects all failures
/// rather than short-circuiting on the first, so a client sees the whole
/// list at once (Spring's `BindingResult` / pydantic's `ValidationError`
/// list semantics). An empty set never escapes the trait: `validate`
/// returns `Ok(())` when nothing failed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl ValidationErrors {
    /// Creates an empty error set.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Appends a violation.
    pub fn push(&mut self, error: ValidationError) {
        self.0.push(error);
    }

    /// Returns `true` when no constraint failed.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of violations gathered.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The violations as a slice.
    pub fn errors(&self) -> &[ValidationError] {
        &self.0
    }

    /// Turns the set into `Err(self)` when non-empty, else `Ok(())` — the
    /// terminal step every generated [`Validate::validate`] body runs.
    pub fn into_result(self) -> Result<(), Self> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(self)
        }
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "validation failed: ")?;
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

impl From<ValidationError> for ValidationErrors {
    fn from(error: ValidationError) -> Self {
        Self(vec![error])
    }
}

impl IntoIterator for ValidationErrors {
    type Item = ValidationError;
    type IntoIter = std::vec::IntoIter<ValidationError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl From<ValidationErrors> for FireflyError {
    /// Renders the violation set as the framework's canonical 422
    /// validation [`FireflyError`] — [`TYPE_VALIDATION`] as the RFC 7807
    /// `type` URI, a summary detail, and a structured `errors` extension
    /// member carrying every `{field, code, message}` so a client gets
    /// machine-readable per-field reasons on the `application/problem+json`
    /// envelope.
    fn from(errors: ValidationErrors) -> Self {
        let detail = if errors.0.len() == 1 {
            errors.0[0].to_string()
        } else {
            format!("{} fields failed validation", errors.0.len())
        };
        let array: Vec<Value> = errors
            .0
            .iter()
            .map(|e| {
                json!({
                    "field": e.field,
                    "code": e.code,
                    "message": e.message,
                })
            })
            .collect();
        FireflyError::new(TYPE_VALIDATION, "Validation Failed", 422, detail)
            .with_field("errors", Value::Array(array))
    }
}

/// A type whose values can be checked against a declarative constraint
/// set — the JSR-380 (`jakarta.validation`) bean-validation contract,
/// ported to Rust.
///
/// Implemented by `#[derive(Validate)]`, which reads each field's
/// `#[validate(...)]` constraints and emits a [`validate`](Validate::validate)
/// body that runs every constraint, gathering all failures into a single
/// [`ValidationErrors`]. Hand implementations are equally welcome: build a
/// [`ValidationErrors`], `push` any violations, and return
/// [`ValidationErrors::into_result`].
///
/// The web tier's `Valid<T>` extractor calls this after JSON decoding and
/// rejects a failing body with a 422 problem via the
/// [`FireflyError`] conversion above.
pub trait Validate {
    /// Runs every constraint, returning `Ok(())` when the value is valid
    /// or `Err(ValidationErrors)` carrying *all* violations otherwise.
    fn validate(&self) -> Result<(), ValidationErrors>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_is_ok_non_empty_is_err() {
        let mut errs = ValidationErrors::new();
        assert!(errs.is_empty());
        assert!(errs.clone().into_result().is_ok());
        errs.push(ValidationError::new("email", "email", "not a valid email"));
        assert_eq!(errs.len(), 1);
        assert!(errs.clone().into_result().is_err());
    }

    #[test]
    fn display_lists_every_violation() {
        let errs = ValidationErrors(vec![
            ValidationError::new("name", "not_empty", "must not be empty"),
            ValidationError::new("age", "range", "must be between 0 and 120"),
        ]);
        let rendered = errs.to_string();
        assert!(rendered.contains("name: must not be empty (not_empty)"));
        assert!(rendered.contains("age: must be between 0 and 120 (range)"));
        assert!(rendered.contains("; "));
    }

    #[test]
    fn converts_to_422_firefly_error_with_structured_errors() {
        let errs = ValidationErrors(vec![
            ValidationError::new("email", "email", "not a valid email"),
            ValidationError::new("name", "not_empty", "must not be empty"),
        ]);
        let fe: FireflyError = errs.into();
        assert_eq!(fe.status, 422);
        assert_eq!(fe.code, TYPE_VALIDATION);
        assert_eq!(fe.detail, "2 fields failed validation");
        let pd = fe.to_problem();
        let array = pd
            .extensions
            .get("errors")
            .and_then(Value::as_array)
            .expect("errors extension array");
        assert_eq!(array.len(), 2);
        assert_eq!(array[0]["field"], json!("email"));
        assert_eq!(array[0]["code"], json!("email"));
        assert_eq!(array[1]["field"], json!("name"));
    }

    #[test]
    fn single_violation_detail_is_the_violation() {
        let errs: ValidationErrors =
            ValidationError::new("amount", "range", "must be between 1 and 100").into();
        let fe: FireflyError = errs.into();
        assert_eq!(fe.status, 422);
        assert!(fe.detail.contains("amount: must be between 1 and 100 (range)"));
    }

    // A hand-written Validate impl, exercising the trait surface the derive
    // macro generates (the derive itself is tested in `firefly-macros`).
    struct Account {
        name: String,
        age: u32,
    }

    impl Validate for Account {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            if self.name.trim().is_empty() {
                errors.push(ValidationError::new(
                    "name",
                    "not_empty",
                    "must not be empty",
                ));
            }
            if !(18..=120).contains(&self.age) {
                errors.push(ValidationError::new(
                    "age",
                    "range",
                    "must be between 18 and 120",
                ));
            }
            errors.into_result()
        }
    }

    #[test]
    fn hand_written_validate_collects_all_failures() {
        let ok = Account {
            name: "ada".into(),
            age: 36,
        };
        assert!(ok.validate().is_ok());

        let bad = Account {
            name: "  ".into(),
            age: 9,
        };
        let errs = bad.validate().expect_err("two failures");
        assert_eq!(errs.len(), 2);
        assert_eq!(errs.errors()[0].field, "name");
        assert_eq!(errs.errors()[1].field, "age");
    }
}
