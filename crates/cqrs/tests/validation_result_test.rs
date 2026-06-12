//! Port of pyfly's `tests/cqrs/test_validation.py` exercising the public
//! structured-validation surface (`ValidationResult` / `ValidationError`
//! / `ValidationSeverity` / `StructuredValidate`) as an external consumer,
//! and proving the structured path bridges to the unchanged
//! `ValidationMiddleware` through `Message::validate`.
//!
//! Python idioms are adapted per the porting contract: pyfly's
//! `AutoValidationProcessor` (which discovers an `obj.validate()` returning
//! a `ValidationResult`) becomes the `StructuredValidate` trait, and
//! `CqrsValidationException` becomes `CqrsError::Validation` (via
//! `ValidationResult::into_cqrs_error`).

use firefly_cqrs::{
    Bus, CqrsError, Message, StructuredValidate, ValidationError, ValidationMiddleware,
    ValidationResult, ValidationSeverity, VALIDATION_ERROR_CODE,
};
use serde::Serialize;

// ── fixtures (pyfly ValidCommand / InvalidCommand) ──────────────────────

/// pyfly `InvalidCommand`: `validate()` fails when `name` is empty.
#[derive(Clone, Serialize)]
struct CreateUser {
    name: String,
}

impl StructuredValidate for CreateUser {
    fn validate_structured(&self) -> ValidationResult {
        if self.name.is_empty() {
            ValidationResult::failure("name", "name is required")
        } else {
            ValidationResult::success()
        }
    }
}

impl Message for CreateUser {
    fn validate(&self) -> Result<(), CqrsError> {
        self.validate_structured().into_cqrs_error()
    }
}

/// A plain message with no structured validation — the default
/// `StructuredValidate` body passes (pyfly's "object without validate()").
#[derive(Clone, Serialize)]
struct Ping;
impl StructuredValidate for Ping {}
impl Message for Ping {}

#[derive(Clone, Debug, PartialEq)]
struct Ack;

// ── ValidationSeverity (pyfly TestValidationSeverity) ───────────────────

#[test]
fn severity_wire_strings_match_pyfly() {
    assert_eq!(ValidationSeverity::Warning.as_str(), "WARNING");
    assert_eq!(ValidationSeverity::Error.as_str(), "ERROR");
    assert_eq!(ValidationSeverity::Critical.as_str(), "CRITICAL");
    assert_eq!(ValidationSeverity::default(), ValidationSeverity::Error);
}

// ── ValidationError / ValidationResult (pyfly Test* classes) ────────────

#[test]
fn failure_carries_field_and_default_code() {
    let r = ValidationResult::failure("email", "invalid email");
    assert!(!r.is_valid());
    assert_eq!(r.errors().len(), 1);
    assert_eq!(r.errors()[0].field_name, "email");
    assert_eq!(r.errors()[0].error_code, VALIDATION_ERROR_CODE);
}

#[test]
fn from_errors_and_combine() {
    assert!(ValidationResult::from_errors(vec![]).is_valid());
    let combined =
        ValidationResult::failure("name", "required").combine(ValidationResult::failure_with(
            ValidationError::new("age", "must be positive")
                .with_severity(ValidationSeverity::Warning),
        ));
    assert!(!combined.is_valid());
    assert_eq!(combined.errors().len(), 2);
    assert_eq!(combined.errors()[1].severity, ValidationSeverity::Warning);
    assert_eq!(
        combined.error_messages(),
        vec!["name: required", "age: must be positive"]
    );
}

// ── StructuredValidate (pyfly TestAutoValidationProcessor) ──────────────

#[test]
fn structured_validate_success_and_failure() {
    let ok = CreateUser { name: "ok".into() };
    assert!(ok.validate_structured().is_valid());

    let bad = CreateUser {
        name: String::new(),
    };
    let result = bad.validate_structured();
    assert!(!result.is_valid());
    assert!(result.errors().iter().any(|e| e.field_name == "name"));
}

#[test]
fn plain_message_passes_structured_validation() {
    assert!(Ping.validate_structured().is_valid());
}

// ── End-to-end: the structured path flows through ValidationMiddleware ──

#[tokio::test]
async fn middleware_rejects_structured_failure_and_accepts_success() {
    let bus = Bus::new();
    bus.use_middleware(ValidationMiddleware::new());
    bus.register(|_c: CreateUser| async move { Ok::<_, CqrsError>(Ack) });

    // Valid command dispatches.
    let ack: Ack = bus
        .send(CreateUser {
            name: "alice".into(),
        })
        .await
        .unwrap();
    assert_eq!(ack, Ack);

    // Invalid command short-circuits with the structured summary folded in.
    let err = bus
        .send::<_, Ack>(CreateUser {
            name: String::new(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CqrsError::Validation(_)));
    assert!(err.to_string().contains("name: name is required"));
}
