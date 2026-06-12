//! Port of pyfly's `tests/kernel/test_exceptions.py` `TestErrorEnums`,
//! `TestErrorResponse` cases for the typed structured-error model, plus
//! Rust-specific coverage (serde round-trips, the always-present /
//! omit-when-empty wire contract). The model is additive over
//! `ProblemDetail`; these tests assert it does NOT change the
//! `problem+json` bytes.

use std::collections::BTreeMap;

use firefly_kernel::{ErrorCategory, ErrorResponse, ErrorSeverity, FieldError, ProblemDetail};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// TestErrorEnums
// ---------------------------------------------------------------------------

#[test]
fn error_category_values() {
    assert_eq!(ErrorCategory::Validation.as_str(), "VALIDATION");
    assert_eq!(ErrorCategory::Business.as_str(), "BUSINESS");
    assert_eq!(ErrorCategory::Technical.as_str(), "TECHNICAL");
    assert_eq!(ErrorCategory::Security.as_str(), "SECURITY");
    assert_eq!(ErrorCategory::External.as_str(), "EXTERNAL");
    assert_eq!(ErrorCategory::Resource.as_str(), "RESOURCE");
    assert_eq!(ErrorCategory::RateLimit.as_str(), "RATE_LIMIT");
    assert_eq!(ErrorCategory::CircuitBreaker.as_str(), "CIRCUIT_BREAKER");
    // The serde value mirrors `.as_str()`.
    assert_eq!(
        serde_json::to_value(ErrorCategory::RateLimit).unwrap(),
        json!("RATE_LIMIT")
    );
}

#[test]
fn error_severity_values() {
    assert_eq!(ErrorSeverity::Low.as_str(), "LOW");
    assert_eq!(ErrorSeverity::Medium.as_str(), "MEDIUM");
    assert_eq!(ErrorSeverity::High.as_str(), "HIGH");
    assert_eq!(ErrorSeverity::Critical.as_str(), "CRITICAL");
    assert_eq!(
        serde_json::to_value(ErrorSeverity::Critical).unwrap(),
        json!("CRITICAL")
    );
}

#[test]
fn enum_defaults_match_pyfly() {
    assert_eq!(ErrorCategory::default(), ErrorCategory::Technical);
    assert_eq!(ErrorSeverity::default(), ErrorSeverity::Medium);
}

// ---------------------------------------------------------------------------
// TestErrorResponse
// ---------------------------------------------------------------------------

#[test]
fn minimal_error_response() {
    let resp = ErrorResponse::new(
        "2026-02-13T12:00:00Z",
        404,
        "Not Found",
        "Order not found",
        "ORDER_NOT_FOUND",
        "/api/orders/123",
    );
    assert_eq!(resp.status, 404);
    assert_eq!(resp.code, "ORDER_NOT_FOUND");
    assert!(!resp.retryable);
    assert_eq!(resp.trace_id, None);
    // pyfly defaults.
    assert_eq!(resp.category, ErrorCategory::Technical);
    assert_eq!(resp.severity, ErrorSeverity::Medium);
}

#[test]
fn error_response_to_dict() {
    let resp = ErrorResponse::new(
        "2026-02-13T12:00:00Z",
        429,
        "Too Many Requests",
        "Rate limit exceeded",
        "RATE_LIMIT",
        "/api/orders",
    )
    .with_category(ErrorCategory::RateLimit)
    .with_severity(ErrorSeverity::Medium)
    .with_retryable(true)
    .with_retry_after(30);

    let d = resp.to_value();
    assert_eq!(d["status"], json!(429));
    assert_eq!(d["category"], json!("RATE_LIMIT"));
    assert_eq!(d["retryable"], json!(true));
    assert_eq!(d["retry_after"], json!(30));
    assert_eq!(d["severity"], json!("MEDIUM"));
}

#[test]
fn error_response_with_validation_errors() {
    let field_errors = vec![
        FieldError::new("email", "Invalid email format").with_rejected_value("not-email"),
        FieldError::new("age", "Must be positive").with_rejected_value("-1"),
    ];
    let resp = ErrorResponse::new(
        "2026-02-13T12:00:00Z",
        422,
        "Validation Error",
        "Input validation failed",
        "VALIDATION_ERROR",
        "/api/users",
    )
    .with_field_errors(field_errors);

    assert_eq!(resp.field_errors.len(), 2);
    assert_eq!(resp.field_errors[0].field, "email");

    let d = resp.to_value();
    assert_eq!(d["field_errors"].as_array().unwrap().len(), 2);
    assert_eq!(d["field_errors"][0]["field"], json!("email"));
    assert_eq!(
        d["field_errors"][0]["message"],
        json!("Invalid email format")
    );
    assert_eq!(d["field_errors"][0]["rejected_value"], json!("not-email"));
}

#[test]
fn error_response_excludes_none_from_dict() {
    let resp = ErrorResponse::new(
        "2026-02-13T12:00:00Z",
        500,
        "Internal Server Error",
        "Something went wrong",
        "INTERNAL_ERROR",
        "/api/test",
    );
    let d = resp.to_value();
    let obj = d.as_object().unwrap();
    assert!(!obj.contains_key("trace_id"));
    assert!(!obj.contains_key("field_errors"));
    assert!(!obj.contains_key("retry_after"));
    assert!(!obj.contains_key("span_id"));
    assert!(!obj.contains_key("transaction_id"));
    assert!(!obj.contains_key("suggestion"));
    assert!(!obj.contains_key("documentation_url"));
    assert!(!obj.contains_key("debug_info"));
}

// ---------------------------------------------------------------------------
// Rust-specific coverage
// ---------------------------------------------------------------------------

#[test]
fn always_present_core_members() {
    let resp = ErrorResponse::new("t", 400, "Bad Request", "bad", "BAD", "/p");
    let d = resp.to_value();
    let obj = d.as_object().unwrap();
    for key in [
        "timestamp",
        "status",
        "error",
        "message",
        "code",
        "path",
        "category",
        "severity",
        "retryable",
    ] {
        assert!(obj.contains_key(key), "missing always-present key {key}");
    }
}

#[test]
fn field_error_default_rejected_value_is_null() {
    let fe = FieldError::new("name", "must not be blank");
    assert_eq!(fe.rejected_value, Value::Null);
    let v = serde_json::to_value(&fe).unwrap();
    assert_eq!(v["rejected_value"], Value::Null);
}

#[test]
fn optional_members_present_when_set() {
    let mut debug = BTreeMap::new();
    debug.insert("attempt".to_string(), json!(3));
    let resp = ErrorResponse::new("t", 503, "Service Unavailable", "down", "DOWN", "/p")
        .with_trace_id("trace-1")
        .with_span_id("span-1")
        .with_transaction_id("txn-1")
        .with_suggestion("retry later")
        .with_documentation_url("https://docs/down")
        .with_debug_info(debug)
        .with_field_error(FieldError::new("f", "m"));

    let d = resp.to_value();
    assert_eq!(d["trace_id"], json!("trace-1"));
    assert_eq!(d["span_id"], json!("span-1"));
    assert_eq!(d["transaction_id"], json!("txn-1"));
    assert_eq!(d["suggestion"], json!("retry later"));
    assert_eq!(d["documentation_url"], json!("https://docs/down"));
    assert_eq!(d["debug_info"]["attempt"], json!(3));
    assert_eq!(d["field_errors"][0]["field"], json!("f"));
}

#[test]
fn serialize_matches_to_value() {
    let resp = ErrorResponse::new("t", 422, "Validation Error", "bad", "BAD", "/p")
        .with_category(ErrorCategory::Validation)
        .with_field_error(FieldError::new("x", "y").with_rejected_value(7));
    let from_string: Value = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
    assert_eq!(from_string, resp.to_value());
}

#[test]
fn problem_detail_wire_bytes_unchanged() {
    // ErrorResponse is additive: ProblemDetail's Go-parity bytes must
    // be byte-for-byte identical to before this change.
    let pd = ProblemDetail::not_found("user 42 missing");
    let data = serde_json::to_string(&pd).expect("marshal");
    assert_eq!(
        data,
        r#"{"detail":"user 42 missing","status":404,"title":"Not Found","type":"https://fireflyframework.org/problems/not-found"}"#
    );
}
