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

//! Port of the Go kernel module's `kernel_test.go`, plus Rust-specific
//! coverage (serde round-trips, exact wire-shape bytes, Send + Sync
//! bounds, source-chain traversal).

use std::error::Error as StdError;

use chrono::{DateTime, Duration, TimeZone, Utc};
use firefly_kernel::{
    as_problem, correlation_id, is_firefly, new_correlation_id, status_of, with_correlation_id,
    with_correlation_id_sync, Clock, FireflyError, FireflyResult, FixedClock, MutableClock,
    ProblemDetail, SystemClock, HEADER_CORRELATION_ID, HEADER_IDEMPOTENCY_KEY,
    PROBLEM_CONTENT_TYPE, TYPE_BAD_REQUEST, TYPE_CONFLICT, TYPE_FORBIDDEN, TYPE_IDEMPOTENCY,
    TYPE_INTERNAL, TYPE_NOT_FOUND, TYPE_RATE_LIMITED, TYPE_UNAUTHORIZED, TYPE_UNPROCESSABLE,
    TYPE_VALIDATION, VERSION,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// TestProblemDetailMarshalRoundtrip
// ---------------------------------------------------------------------------

#[test]
fn problem_detail_marshal_roundtrip() {
    let pd = ProblemDetail::not_found("user 42 missing")
        .with_instance("/users/42")
        .with("traceId", "abc-123")
        .with("retryable", false);

    let data = serde_json::to_string(&pd).expect("marshal");
    let got: Value = serde_json::from_str(&data).expect("unmarshal");
    assert_eq!(got["status"], json!(404));
    assert_eq!(
        got["traceId"],
        json!("abc-123"),
        "traceId extension lost: {got}"
    );
    assert_eq!(got["instance"], json!("/users/42"), "instance lost: {got}");
    assert_eq!(got["retryable"], json!(false));

    let rt: ProblemDetail = serde_json::from_str(&data).expect("unmarshal back");
    assert_eq!(rt.status, 404, "roundtrip: {rt:?}");
    assert_eq!(rt.title, "Not Found", "roundtrip: {rt:?}");
    assert_eq!(
        rt.extensions.get("traceId"),
        Some(&json!("abc-123")),
        "extension lost on roundtrip: {:?}",
        rt.extensions
    );
    assert_eq!(rt, pd, "full struct roundtrip");
}

// Rust-specific: the serialized bytes match the Go port exactly —
// lexicographically ordered keys, empty members omitted.
#[test]
fn problem_detail_wire_shape_matches_go() {
    let pd = ProblemDetail::not_found("user 42 missing");
    let data = serde_json::to_string(&pd).expect("marshal");
    assert_eq!(
        data,
        r#"{"detail":"user 42 missing","status":404,"title":"Not Found","type":"https://fireflyframework.org/problems/not-found"}"#
    );
}

// Rust-specific regression: Go marshals ProblemDetail through
// json.Marshal, which HTML-escapes by default, so `<`, `>`, `&` must
// serialize as the u003c/u003e/u0026 Unicode escapes for the wire
// bytes to stay identical to the Go port. The expected strings below
// are the verbatim output of the Go kernel module (go1.26).
#[test]
fn problem_detail_html_escaping_matches_go() {
    let pd = ProblemDetail::bad_request("value <script> & \"quotes\"");
    let data = serde_json::to_string(&pd).expect("marshal");
    assert_eq!(
        data,
        concat!(
            "{\"detail\":\"value \\u003cscript\\u003e \\u0026 \\\"quotes\\\"\",",
            "\"status\":400,\"title\":\"Bad Request\",",
            "\"type\":\"https://fireflyframework.org/problems/bad-request\"}"
        )
    );

    // The escapes decode back to the original characters.
    let rt: ProblemDetail = serde_json::from_str(&data).expect("unmarshal back");
    assert_eq!(rt, pd, "escaped wire form must roundtrip");
}

// Go escapes every string in the document — extension keys and nested
// extension values included.
#[test]
fn problem_detail_html_escaping_covers_keys_and_nested_extensions() {
    let pd = ProblemDetail::new("t<&>", "T<>", 422, "d")
        .with_instance("/p?a=1&b=2")
        .with("err<s>", json!(["a&b", {"k<": "v>"}]))
        .with("plain", 7);
    assert_eq!(
        serde_json::to_string(&pd).expect("marshal"),
        concat!(
            "{\"detail\":\"d\",",
            "\"err\\u003cs\\u003e\":[\"a\\u0026b\",{\"k\\u003c\":\"v\\u003e\"}],",
            "\"instance\":\"/p?a=1\\u0026b=2\",\"plain\":7,\"status\":422,",
            "\"title\":\"T\\u003c\\u003e\",\"type\":\"t\\u003c\\u0026\\u003e\"}"
        )
    );
}

// Control characters use Go's shorthand escapes where they exist and
// lowercase u00xx escapes otherwise; DEL passes through raw; the JS
// line/paragraph separators U+2028/U+2029 escape to u2028/u2029.
#[test]
fn problem_detail_control_and_separator_escaping_matches_go() {
    let pd = ProblemDetail::new("", "", 0, "a\nb\rc\td\u{8}e\u{c}f\u{1}g\u{7f}h");
    assert_eq!(
        serde_json::to_string(&pd).expect("marshal"),
        "{\"detail\":\"a\\nb\\rc\\td\\be\\ff\\u0001g\u{7f}h\"}"
    );

    let pd = ProblemDetail::new("", "", 0, "x\u{2028}y\u{2029}z");
    assert_eq!(
        serde_json::to_string(&pd).expect("marshal"),
        "{\"detail\":\"x\\u2028y\\u2029z\"}"
    );
}

// serde_json::to_value must keep producing the semantic (unescaped)
// document even though Serialize pre-renders escaped JSON text.
#[test]
fn problem_detail_to_value_decodes_escapes() {
    let pd = ProblemDetail::bad_request("a & b <c>").with("k<", "v>");
    let got: Value = serde_json::to_value(&pd).expect("to_value");
    assert_eq!(got["detail"], json!("a & b <c>"));
    assert_eq!(got["k<"], json!("v>"));
    assert_eq!(got["status"], json!(400));
}

#[test]
fn problem_detail_omits_empty_members() {
    let data = serde_json::to_string(&ProblemDetail::default()).expect("marshal");
    assert_eq!(data, "{}");

    let pd = ProblemDetail::new("", "", 0, "only-detail");
    let got: Value = serde_json::to_value(&pd).expect("to_value");
    let obj = got.as_object().expect("object");
    assert_eq!(obj.len(), 1);
    assert_eq!(obj["detail"], json!("only-detail"));
}

#[test]
fn problem_detail_standard_members_win_on_collision() {
    // RFC 7807 §3.2: extensions must not clobber standard members.
    let pd = ProblemDetail::conflict("dup")
        .with("status", 999)
        .with("title", "shadow");
    let got: Value = serde_json::to_value(&pd).expect("to_value");
    assert_eq!(got["status"], json!(409));
    assert_eq!(got["title"], json!("Conflict"));
}

#[test]
fn problem_detail_unmarshal_wrong_typed_member_stays_extension() {
    // Go only lifts standard members of the right JSON type; others
    // remain in Extensions.
    let pd: ProblemDetail =
        serde_json::from_str(r#"{"type":5,"status":"oops","detail":"x"}"#).expect("unmarshal");
    assert_eq!(pd.problem_type, "");
    assert_eq!(pd.status, 0);
    assert_eq!(pd.detail, "x");
    assert_eq!(pd.extensions.get("type"), Some(&json!(5)));
    assert_eq!(pd.extensions.get("status"), Some(&json!("oops")));
}

#[test]
fn problem_builders_emit_canonical_types() {
    let cases: Vec<(ProblemDetail, &str, &str, u16)> = vec![
        (
            ProblemDetail::bad_request("d"),
            TYPE_BAD_REQUEST,
            "Bad Request",
            400,
        ),
        (
            ProblemDetail::unauthorized("d"),
            TYPE_UNAUTHORIZED,
            "Unauthorized",
            401,
        ),
        (
            ProblemDetail::forbidden("d"),
            TYPE_FORBIDDEN,
            "Forbidden",
            403,
        ),
        (
            ProblemDetail::not_found("d"),
            TYPE_NOT_FOUND,
            "Not Found",
            404,
        ),
        (ProblemDetail::conflict("d"), TYPE_CONFLICT, "Conflict", 409),
        (
            ProblemDetail::unprocessable("d"),
            TYPE_UNPROCESSABLE,
            "Unprocessable Entity",
            422,
        ),
        (
            ProblemDetail::rate_limited("d"),
            TYPE_RATE_LIMITED,
            "Too Many Requests",
            429,
        ),
        (
            ProblemDetail::internal("d"),
            TYPE_INTERNAL,
            "Internal Server Error",
            500,
        ),
        (
            ProblemDetail::validation("d"),
            TYPE_VALIDATION,
            "Validation Failed",
            422,
        ),
    ];
    for (pd, typ, title, status) in cases {
        assert_eq!(pd.problem_type, typ);
        assert_eq!(pd.title, title);
        assert_eq!(pd.status, status);
        assert_eq!(pd.detail, "d");
    }
    assert_eq!(PROBLEM_CONTENT_TYPE, "application/problem+json");
}

// ---------------------------------------------------------------------------
// TestResultBasic — adapted: Go's Result[T] maps to FireflyResult<T>
// (std Result), so map / and_then replace MapResult / FlatMapResult.
// ---------------------------------------------------------------------------

#[test]
// The literal Ok/Err constructions intentionally mirror Go's
// TestResultBasic table; clippy rightly notes they could be simplified.
#[allow(
    clippy::unnecessary_literal_unwrap,
    clippy::bind_instead_of_map,
    clippy::nonminimal_bool
)]
fn result_basic() {
    let ok: FireflyResult<i32> = Ok(42);
    assert!(ok.is_ok() && !ok.is_err(), "Ok must report is_ok");
    assert_eq!(ok.unwrap(), 42);

    let bad: FireflyResult<i32> = Err(FireflyError::internal("nope"));
    assert!(bad.is_err(), "Err must not be Ok");
    let err = bad.unwrap_err();
    assert_eq!(err.detail, "nope", "Err must propagate cause: {err}");

    let mapped: FireflyResult<String> = Ok(2).map(|x: i32| "x".repeat(x as usize));
    assert_eq!(mapped.unwrap(), "xx");

    let chain: FireflyResult<i32> = Ok(3).and_then(|x: i32| Ok(x * 10));
    assert_eq!(chain.unwrap(), 30);
}

// ---------------------------------------------------------------------------
// TestFireflyErrorAndProblem
// ---------------------------------------------------------------------------

#[test]
fn firefly_error_and_problem() {
    let fe = FireflyError::not_found("missing").with_field("resource", "user");
    assert_eq!(fe.status, 404);
    let pd = fe.to_problem();
    assert_eq!(
        pd.problem_type, TYPE_NOT_FOUND,
        "problem mapping wrong: {pd:?}"
    );
    assert_eq!(
        pd.extensions.get("resource"),
        Some(&json!("user")),
        "problem mapping wrong: {pd:?}"
    );
    assert!(is_firefly(&fe), "is_firefly must recognise FireflyError");
    assert_eq!(
        status_of(&fe),
        404,
        "status_of must read FireflyError.status"
    );

    let plain = std::io::Error::other("x");
    assert_eq!(status_of(&plain), 500, "status_of default must be 500");

    // as_problem dispatches on type.
    assert_eq!(as_problem(&fe).problem_type, TYPE_NOT_FOUND);
    let boom = std::io::Error::other("boom");
    let pd = as_problem(&boom);
    assert_eq!(
        pd.problem_type, TYPE_INTERNAL,
        "as_problem(other) must default to internal"
    );
    assert_eq!(pd.detail, "boom");
}

#[test]
fn firefly_error_constructors() {
    let cases: Vec<(FireflyError, &str, &str, u16)> = vec![
        (
            FireflyError::bad_request("d"),
            TYPE_BAD_REQUEST,
            "Bad Request",
            400,
        ),
        (
            FireflyError::unauthorized("d"),
            TYPE_UNAUTHORIZED,
            "Unauthorized",
            401,
        ),
        (
            FireflyError::forbidden("d"),
            TYPE_FORBIDDEN,
            "Forbidden",
            403,
        ),
        (
            FireflyError::not_found("d"),
            TYPE_NOT_FOUND,
            "Not Found",
            404,
        ),
        (FireflyError::conflict("d"), TYPE_CONFLICT, "Conflict", 409),
        (
            FireflyError::validation("d"),
            TYPE_VALIDATION,
            "Validation Failed",
            422,
        ),
        (
            FireflyError::rate_limited("d"),
            TYPE_RATE_LIMITED,
            "Too Many Requests",
            429,
        ),
        (
            FireflyError::internal("d"),
            TYPE_INTERNAL,
            "Internal Server Error",
            500,
        ),
        (
            FireflyError::idempotency_conflict("d"),
            TYPE_IDEMPOTENCY,
            "Idempotency Conflict",
            409,
        ),
    ];
    for (fe, code, title, status) in cases {
        assert_eq!(fe.code, code);
        assert_eq!(fe.title, title);
        assert_eq!(fe.status, status);
        assert_eq!(fe.detail, "d");
    }
}

#[test]
fn firefly_error_display_matches_go() {
    // Go renders "code: detail" — or "code: title" when detail is empty.
    let with_detail = FireflyError::not_found("user 42 missing");
    assert_eq!(
        with_detail.to_string(),
        "https://fireflyframework.org/problems/not-found: user 42 missing"
    );
    let without_detail = FireflyError::not_found("");
    assert_eq!(
        without_detail.to_string(),
        "https://fireflyframework.org/problems/not-found: Not Found"
    );
}

#[test]
fn firefly_error_cause_chain() {
    let io = std::io::Error::other("disk on fire");
    let fe = FireflyError::internal("storage failed").with_cause(io);
    let source = fe.source().expect("source must expose the cause");
    assert_eq!(source.to_string(), "disk on fire");

    // status_of / is_firefly walk the source chain, like Go's errors.As.
    #[derive(Debug)]
    struct Wrapper(FireflyError);
    impl std::fmt::Display for Wrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "wrapped: {}", self.0)
        }
    }
    impl StdError for Wrapper {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }
    let wrapped = Wrapper(FireflyError::not_found("missing"));
    assert!(is_firefly(&wrapped));
    assert_eq!(status_of(&wrapped), 404);
    assert_eq!(as_problem(&wrapped).problem_type, TYPE_NOT_FOUND);
}

// ---------------------------------------------------------------------------
// TestClockMutable
// ---------------------------------------------------------------------------

#[test]
fn clock_mutable() {
    let epoch: DateTime<Utc> = DateTime::UNIX_EPOCH;
    let mc = MutableClock::new(epoch);
    assert_eq!(mc.now(), epoch, "initial: {}", mc.now());
    mc.advance(Duration::hours(1));
    assert_eq!(
        mc.now() - epoch,
        Duration::hours(1),
        "after advance: {}",
        mc.now()
    );
    mc.set(Utc.timestamp_opt(1_000, 0).unwrap());
    assert_eq!(mc.now().timestamp(), 1_000);

    let fc = FixedClock(Utc.timestamp_opt(123, 0).unwrap());
    assert_eq!(fc.now().timestamp(), 123, "fixed clock wrong");

    assert!(SystemClock.now() > epoch, "system clock returned zero time");
}

// Rust-specific: the trait is object-safe and the default MutableClock
// starts at the Unix epoch.
#[test]
fn clock_is_object_safe() {
    let clocks: Vec<Box<dyn Clock>> = vec![
        Box::new(SystemClock),
        Box::new(FixedClock(DateTime::UNIX_EPOCH)),
        Box::new(MutableClock::default()),
    ];
    assert_eq!(clocks[2].now(), DateTime::UNIX_EPOCH);
    for c in &clocks {
        let _ = c.now();
    }
}

// ---------------------------------------------------------------------------
// TestCorrelationContext
// ---------------------------------------------------------------------------

#[tokio::test]
async fn correlation_context() {
    assert!(
        correlation_id().is_none(),
        "empty scope must not yield correlation id"
    );

    let id = new_correlation_id();
    assert_eq!(id.len(), 32, "correlation id length: {}", id.len());
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit()),
        "correlation id not hex: {id}"
    );

    let got = with_correlation_id(id.clone(), async { correlation_id() }).await;
    assert_eq!(got, Some(id), "retrieve");
}

#[tokio::test]
async fn correlation_empty_id_yields_none() {
    // Go's CorrelationIDFrom returns ok=false for an empty stored id.
    let got = with_correlation_id("", async { correlation_id() }).await;
    assert!(got.is_none());
}

#[tokio::test]
async fn correlation_scopes_nest_like_child_contexts() {
    let got = with_correlation_id("outer", async {
        let inner = with_correlation_id("inner", async { correlation_id() }).await;
        (inner, correlation_id())
    })
    .await;
    assert_eq!(got, (Some("inner".to_owned()), Some("outer".to_owned())));
}

#[test]
fn correlation_sync_scope() {
    assert!(correlation_id().is_none());
    let got = with_correlation_id_sync("sync-123", correlation_id);
    assert_eq!(got, Some("sync-123".to_owned()));
}

#[test]
fn header_names_match_go() {
    assert_eq!(HEADER_CORRELATION_ID, "X-Correlation-Id");
    assert_eq!(HEADER_IDEMPOTENCY_KEY, "Idempotency-Key");
}

// ---------------------------------------------------------------------------
// Version + Rust-specific bounds
// ---------------------------------------------------------------------------

#[test]
fn version_is_stamped() {
    assert_eq!(VERSION, "26.6.16");
}

#[test]
fn kernel_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FireflyError>();
    assert_send_sync::<FireflyResult<i32>>();
    assert_send_sync::<ProblemDetail>();
    assert_send_sync::<SystemClock>();
    assert_send_sync::<FixedClock>();
    assert_send_sync::<MutableClock>();
    assert_send_sync::<Box<dyn Clock>>();
}
