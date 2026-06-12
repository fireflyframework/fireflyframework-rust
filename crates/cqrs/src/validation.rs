//! Structured validation result types — pyfly's `cqrs.validation` package.
//!
//! The Rust port of pyfly's `pyfly.cqrs.validation.types`
//! ([`ValidationResult`] / [`ValidationError`] / [`ValidationSeverity`])
//! plus its `CqrsValidationException`. They model a *richer* validation
//! outcome than the terse [`Message::validate`](crate::Message::validate)
//! hook (which returns `Result<(), CqrsError>`): a [`ValidationResult`]
//! accumulates multiple [`ValidationError`]s, each carrying a
//! `field_name`, `message`, machine-readable `error_code`, a
//! [`ValidationSeverity`], and the optionally-rejected value.
//!
//! This is **additive**: the simple `validate()` path keeps working
//! untouched. Messages that want the structured shape implement the
//! parallel [`StructuredValidate::validate_structured`] hook, which the
//! default [`Message::validate`] adaptor (see
//! [`ValidationResult::into_cqrs_error`]) can fold back into the existing
//! `CqrsError::Validation` channel so structured-validating messages still
//! flow through the unchanged [`ValidationMiddleware`](crate::ValidationMiddleware).
//!
//! The wire shape mirrors pyfly exactly: [`ValidationSeverity`] serializes
//! to the same `"WARNING"` / `"ERROR"` / `"CRITICAL"` strings, and the
//! default error code is [`VALIDATION_ERROR_CODE`] (`"VALIDATION_ERROR"`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::CqrsError;

/// Default error code stamped on a [`ValidationError`] — pyfly's
/// `error_code: str = "VALIDATION_ERROR"`.
pub const VALIDATION_ERROR_CODE: &str = "VALIDATION_ERROR";

/// Severity level for a [`ValidationError`] — pyfly's `ValidationSeverity`
/// StrEnum. Serializes to the same wire strings (`"WARNING"` / `"ERROR"` /
/// `"CRITICAL"`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ValidationSeverity {
    /// Advisory: logged but does not necessarily fail the dispatch.
    Warning,
    /// Standard validation failure (the default).
    #[default]
    Error,
    /// Severe failure worth alerting on.
    Critical,
}

impl ValidationSeverity {
    /// The wire string for the severity — pyfly's StrEnum `.value`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
            Self::Critical => "CRITICAL",
        }
    }
}

impl fmt::Display for ValidationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single validation failure — pyfly's frozen `ValidationError`
/// dataclass (`field_name`, `message`, `error_code`, `severity`,
/// `rejected_value`).
///
/// Build one with [`ValidationError::new`] (pyfly defaults: code
/// [`VALIDATION_ERROR_CODE`], severity [`ValidationSeverity::Error`], no
/// rejected value) and refine it with the chainable
/// `with_*` setters mirroring pyfly's keyword arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationError {
    /// Name of the offending field, e.g. `"email"`.
    pub field_name: String,
    /// Human-readable failure message.
    pub message: String,
    /// Machine-readable error code; defaults to [`VALIDATION_ERROR_CODE`].
    pub error_code: String,
    /// Severity; defaults to [`ValidationSeverity::Error`].
    pub severity: ValidationSeverity,
    /// The value that was rejected, when the validator chose to surface it;
    /// omitted from JSON when absent — pyfly's `rejected_value: object = None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_value: Option<serde_json::Value>,
}

impl ValidationError {
    /// Builds an error with the pyfly defaults: code
    /// [`VALIDATION_ERROR_CODE`], severity [`ValidationSeverity::Error`],
    /// no rejected value.
    pub fn new(field_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field_name: field_name.into(),
            message: message.into(),
            error_code: VALIDATION_ERROR_CODE.to_string(),
            severity: ValidationSeverity::Error,
            rejected_value: None,
        }
    }

    /// Replaces the error code — pyfly's `error_code=` keyword.
    #[must_use]
    pub fn with_error_code(mut self, error_code: impl Into<String>) -> Self {
        self.error_code = error_code.into();
        self
    }

    /// Replaces the severity — pyfly's `severity=` keyword.
    #[must_use]
    pub fn with_severity(mut self, severity: ValidationSeverity) -> Self {
        self.severity = severity;
        self
    }

    /// Records the rejected value — pyfly's `rejected_value=` keyword.
    #[must_use]
    pub fn with_rejected_value(mut self, value: serde_json::Value) -> Self {
        self.rejected_value = Some(value);
        self
    }
}

/// Immutable result of a validation operation — pyfly's `ValidationResult`
/// frozen dataclass. Compose multiple results with
/// [`ValidationResult::combine`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationResult {
    valid: bool,
    errors: Vec<ValidationError>,
    summary: Option<String>,
}

impl ValidationResult {
    /// A valid result with no errors — pyfly's `ValidationResult.success()`.
    pub fn success() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            summary: None,
        }
    }

    /// A failure carrying a single error with the default code and
    /// severity — pyfly's `ValidationResult.failure(field_name, message)`.
    /// Use [`ValidationResult::failure_with`] to customise the error.
    pub fn failure(field_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::failure_with(ValidationError::new(field_name, message))
    }

    /// A failure carrying the given fully-specified error — covers pyfly's
    /// `failure(..., error_code=...)` keyword and any
    /// [`ValidationError`] customised via its `with_*` setters.
    pub fn failure_with(error: ValidationError) -> Self {
        Self {
            valid: false,
            errors: vec![error],
            summary: None,
        }
    }

    /// Builds a result from a list of errors — pyfly's
    /// `ValidationResult.from_errors(errors)`. An empty list yields
    /// [`ValidationResult::success`].
    pub fn from_errors(errors: Vec<ValidationError>) -> Self {
        if errors.is_empty() {
            return Self::success();
        }
        Self {
            valid: false,
            errors,
            summary: None,
        }
    }

    /// Whether the validation passed — pyfly's `.valid` field.
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// The accumulated errors (empty on success).
    pub fn errors(&self) -> &[ValidationError] {
        &self.errors
    }

    /// The optional human-readable summary.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Sets the summary used by the [`fmt::Display`] rendering and by
    /// [`ValidationResult::into_cqrs_error`].
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// Merges two results: invalid if either is invalid, errors
    /// concatenated — pyfly's `combine` (the summary is reset, exactly like
    /// pyfly's).
    #[must_use]
    pub fn combine(mut self, other: ValidationResult) -> Self {
        self.errors.extend(other.errors);
        Self {
            valid: self.valid && other.valid,
            errors: self.errors,
            summary: None,
        }
    }

    /// Renders each error as `"<field_name>: <message>"` — pyfly's
    /// `error_messages()`.
    pub fn error_messages(&self) -> Vec<String> {
        self.errors
            .iter()
            .map(|e| format!("{}: {}", e.field_name, e.message))
            .collect()
    }

    /// Folds a failed result into a [`CqrsError::Validation`], so a
    /// structured-validating message still short-circuits the unchanged
    /// [`ValidationMiddleware`](crate::ValidationMiddleware) through the
    /// existing simple [`Message::validate`](crate::Message::validate)
    /// channel.
    ///
    /// A valid result yields `Ok(())`. An invalid result yields
    /// `Err(CqrsError::Validation(msg))`, where `msg` is the explicit
    /// summary if set, else the joined [`ValidationResult::error_messages`],
    /// else `"Validation failed"` — pyfly's `CqrsValidationException`
    /// message derivation.
    pub fn into_cqrs_error(self) -> Result<(), CqrsError> {
        if self.valid {
            return Ok(());
        }
        Err(CqrsError::validation(self.to_string()))
    }
}

impl fmt::Display for ValidationResult {
    /// The failure summary used by [`ValidationResult::into_cqrs_error`]:
    /// the explicit summary if set, else the joined
    /// [`ValidationResult::error_messages`], else `"Validation failed"` —
    /// exactly pyfly's `CqrsValidationException` message derivation. A
    /// valid result renders as `"valid"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.valid {
            return f.write_str("valid");
        }
        if let Some(summary) = &self.summary {
            return f.write_str(summary);
        }
        let messages = self.error_messages();
        if messages.is_empty() {
            f.write_str("Validation failed")
        } else {
            f.write_str(&messages.join("; "))
        }
    }
}

/// Optional richer-validation hook for [`Message`](crate::Message)s that
/// want the structured [`ValidationResult`] shape — the parallel to the
/// terse [`Message::validate`](crate::Message::validate) hook, mirroring
/// pyfly's `obj.validate()` returning a `ValidationResult`.
///
/// Implement this on a message to accumulate multiple
/// [`ValidationError`]s; then either consult it directly, or bridge it to
/// the existing middleware by overriding `Message::validate` to call
/// `self.validate_structured().into_cqrs_error()`.
///
/// This trait is **additive and entirely opt-in**: the [`Bus`](crate::Bus)
/// and [`ValidationMiddleware`](crate::ValidationMiddleware) never require
/// it, so existing handlers are unaffected.
pub trait StructuredValidate {
    /// Produces the structured validation outcome for `self`. The default
    /// returns [`ValidationResult::success`], so plain implementors pass.
    fn validate_structured(&self) -> ValidationResult {
        ValidationResult::success()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ValidationSeverity (pyfly TestValidationSeverity) ───────────

    #[test]
    fn severity_wire_values_match_pyfly() {
        assert_eq!(ValidationSeverity::Warning.as_str(), "WARNING");
        assert_eq!(ValidationSeverity::Error.as_str(), "ERROR");
        assert_eq!(ValidationSeverity::Critical.as_str(), "CRITICAL");
        // Serializes to the SCREAMING_SNAKE_CASE strings pyfly's StrEnum uses.
        for (sev, want) in [
            (ValidationSeverity::Warning, "\"WARNING\""),
            (ValidationSeverity::Error, "\"ERROR\""),
            (ValidationSeverity::Critical, "\"CRITICAL\""),
        ] {
            assert_eq!(serde_json::to_string(&sev).unwrap(), want);
            let back: ValidationSeverity = serde_json::from_str(want).unwrap();
            assert_eq!(back, sev);
        }
    }

    #[test]
    fn severity_default_is_error() {
        assert_eq!(ValidationSeverity::default(), ValidationSeverity::Error);
        assert_eq!(ValidationSeverity::Error.to_string(), "ERROR");
    }

    // ── ValidationError (pyfly TestValidationError) ─────────────────

    #[test]
    fn error_required_fields() {
        let e = ValidationError::new("email", "invalid format");
        assert_eq!(e.field_name, "email");
        assert_eq!(e.message, "invalid format");
    }

    #[test]
    fn error_default_error_code() {
        let e = ValidationError::new("name", "required");
        assert_eq!(e.error_code, "VALIDATION_ERROR");
    }

    #[test]
    fn error_custom_error_code() {
        let e = ValidationError::new("age", "too young").with_error_code("MIN_AGE");
        assert_eq!(e.error_code, "MIN_AGE");
    }

    #[test]
    fn error_default_severity() {
        let e = ValidationError::new("name", "required");
        assert_eq!(e.severity, ValidationSeverity::Error);
    }

    #[test]
    fn error_custom_severity() {
        let e =
            ValidationError::new("notes", "too long").with_severity(ValidationSeverity::Warning);
        assert_eq!(e.severity, ValidationSeverity::Warning);
    }

    #[test]
    fn error_rejected_value_default_none() {
        let e = ValidationError::new("name", "required");
        assert!(e.rejected_value.is_none());
    }

    #[test]
    fn error_rejected_value_can_be_set() {
        let e = ValidationError::new("age", "must be positive")
            .with_rejected_value(serde_json::json!(-5));
        assert_eq!(e.rejected_value, Some(serde_json::json!(-5)));
    }

    #[test]
    fn error_wire_shape_round_trips() {
        let e = ValidationError::new("email", "invalid")
            .with_error_code("INVALID_EMAIL")
            .with_severity(ValidationSeverity::Critical)
            .with_rejected_value(serde_json::json!("bad@"));
        let got = serde_json::to_string(&e).unwrap();
        let want = r#"{"field_name":"email","message":"invalid","error_code":"INVALID_EMAIL","severity":"CRITICAL","rejected_value":"bad@"}"#;
        assert_eq!(got, want);
        let back: ValidationError = serde_json::from_str(&got).unwrap();
        assert_eq!(back, e);
        // Default omits the rejected_value.
        assert_eq!(
            serde_json::to_string(&ValidationError::new("name", "required")).unwrap(),
            r#"{"field_name":"name","message":"required","error_code":"VALIDATION_ERROR","severity":"ERROR"}"#
        );
    }

    // ── ValidationResult (pyfly TestValidationResult) ───────────────

    #[test]
    fn success_is_valid() {
        let r = ValidationResult::success();
        assert!(r.is_valid());
        assert!(r.errors().is_empty());
        assert!(r.summary().is_none());
    }

    #[test]
    fn failure_is_invalid() {
        let r = ValidationResult::failure("email", "invalid email");
        assert!(!r.is_valid());
        assert_eq!(r.errors().len(), 1);
        assert_eq!(r.errors()[0].field_name, "email");
        assert_eq!(r.errors()[0].message, "invalid email");
    }

    #[test]
    fn failure_with_custom_error_code() {
        let r = ValidationResult::failure_with(
            ValidationError::new("email", "bad format").with_error_code("INVALID_EMAIL"),
        );
        assert_eq!(r.errors()[0].error_code, "INVALID_EMAIL");
    }

    #[test]
    fn failure_default_error_code() {
        let r = ValidationResult::failure("name", "required");
        assert_eq!(r.errors()[0].error_code, "VALIDATION_ERROR");
    }

    #[test]
    fn from_errors_empty_list_returns_success() {
        let r = ValidationResult::from_errors(vec![]);
        assert!(r.is_valid());
        assert!(r.errors().is_empty());
    }

    #[test]
    fn from_errors_with_errors() {
        let r = ValidationResult::from_errors(vec![
            ValidationError::new("name", "required"),
            ValidationError::new("email", "invalid"),
        ]);
        assert!(!r.is_valid());
        assert_eq!(r.errors().len(), 2);
    }

    #[test]
    fn combine_both_valid() {
        let combined = ValidationResult::success().combine(ValidationResult::success());
        assert!(combined.is_valid());
        assert!(combined.errors().is_empty());
    }

    #[test]
    fn combine_first_invalid() {
        let combined =
            ValidationResult::failure("name", "required").combine(ValidationResult::success());
        assert!(!combined.is_valid());
        assert_eq!(combined.errors().len(), 1);
    }

    #[test]
    fn combine_second_invalid() {
        let combined =
            ValidationResult::success().combine(ValidationResult::failure("email", "invalid"));
        assert!(!combined.is_valid());
        assert_eq!(combined.errors().len(), 1);
    }

    #[test]
    fn combine_both_invalid_merges_errors() {
        let combined = ValidationResult::failure("name", "required")
            .combine(ValidationResult::failure("email", "invalid"));
        assert!(!combined.is_valid());
        assert_eq!(combined.errors().len(), 2);
        let fields: std::collections::HashSet<&str> = combined
            .errors()
            .iter()
            .map(|e| e.field_name.as_str())
            .collect();
        assert_eq!(fields, std::collections::HashSet::from(["name", "email"]));
    }

    #[test]
    fn error_messages_single() {
        let r = ValidationResult::failure("email", "invalid format");
        assert_eq!(r.error_messages(), vec!["email: invalid format"]);
    }

    #[test]
    fn error_messages_multiple() {
        let combined = ValidationResult::failure("name", "required")
            .combine(ValidationResult::failure("age", "must be positive"));
        let messages = combined.error_messages();
        assert_eq!(messages.len(), 2);
        assert!(messages.contains(&"name: required".to_string()));
        assert!(messages.contains(&"age: must be positive".to_string()));
    }

    #[test]
    fn error_messages_empty_on_success() {
        assert!(ValidationResult::success().error_messages().is_empty());
    }

    #[test]
    fn result_round_trips() {
        let r = ValidationResult::failure("name", "required")
            .combine(ValidationResult::failure("email", "invalid"));
        let json = serde_json::to_string(&r).unwrap();
        let back: ValidationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    // ── into_cqrs_error (pyfly TestCqrsValidationException) ──────────

    #[test]
    fn into_cqrs_error_success_is_ok() {
        assert!(ValidationResult::success().into_cqrs_error().is_ok());
    }

    #[test]
    fn into_cqrs_error_message_from_error_messages() {
        let err = ValidationResult::failure("email", "invalid format")
            .into_cqrs_error()
            .unwrap_err();
        assert!(matches!(err, CqrsError::Validation(_)));
        assert!(err.to_string().contains("email: invalid format"));
    }

    #[test]
    fn into_cqrs_error_custom_summary() {
        let err = ValidationResult::failure("name", "required")
            .with_summary("Custom validation message")
            .into_cqrs_error()
            .unwrap_err();
        assert!(err.to_string().contains("Custom validation message"));
    }

    #[test]
    fn display_renders_valid_and_fallback() {
        assert_eq!(ValidationResult::success().to_string(), "valid");
        // No errors and no summary on an invalid result → the pyfly fallback.
        let invalid = ValidationResult {
            valid: false,
            errors: Vec::new(),
            summary: None,
        };
        assert_eq!(invalid.to_string(), "Validation failed");
    }

    // ── StructuredValidate hook bridges to the simple validate() path ─

    #[derive(Clone, serde::Serialize)]
    struct CreateUser {
        name: String,
        email: String,
    }

    impl StructuredValidate for CreateUser {
        fn validate_structured(&self) -> ValidationResult {
            let mut result = ValidationResult::success();
            if self.name.is_empty() {
                result = result.combine(ValidationResult::failure("name", "name is required"));
            }
            if !self.email.contains('@') {
                result = result.combine(ValidationResult::failure_with(
                    ValidationError::new("email", "invalid email").with_error_code("INVALID_EMAIL"),
                ));
            }
            result
        }
    }

    impl crate::Message for CreateUser {
        fn validate(&self) -> Result<(), CqrsError> {
            self.validate_structured().into_cqrs_error()
        }
    }

    #[test]
    fn structured_validate_default_passes() {
        #[derive(Clone, serde::Serialize)]
        struct Plain;
        impl StructuredValidate for Plain {}
        assert!(Plain.validate_structured().is_valid());
    }

    #[test]
    fn structured_validate_bridges_to_message_validate() {
        let ok = CreateUser {
            name: "alice".into(),
            email: "a@x.io".into(),
        };
        assert!(crate::Message::validate(&ok).is_ok());

        let bad = CreateUser {
            name: String::new(),
            email: "nope".into(),
        };
        let structured = bad.validate_structured();
        assert!(!structured.is_valid());
        assert_eq!(structured.errors().len(), 2);

        let err = crate::Message::validate(&bad).unwrap_err();
        assert!(matches!(err, CqrsError::Validation(_)));
        assert!(err.to_string().contains("name: name is required"));
        assert!(err.to_string().contains("email: invalid email"));
    }
}
