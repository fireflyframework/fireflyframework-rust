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

//! Validator tests, ported from the Go module's `pipeline_test.go`
//! (`TestHMACValidator`) plus cross-crate compatibility evidence: every
//! canonical validator must accept header values produced by the
//! `firefly-testkit` signers, which emit the Go port's exact wire
//! formats.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use http::HeaderMap;

use firefly_kernel::FixedClock;
use firefly_testkit::{sign_github, sign_hmac, sign_stripe, sign_twilio};
use firefly_webhooks::{
    GitHubValidator, HmacValidator, Inbound, StripeValidator, TwilioValidator, Validator,
    WebhookError,
};

fn headers_with(name: &str, value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
        value.parse().expect("header value"),
    );
    headers
}

// --- Go: TestHMACValidator ---------------------------------------------------

#[test]
fn hmac_validator_accepts_valid_and_rejects_tampered() {
    let v = HmacValidator::new("generic", b"s3cret");
    let body = br#"{"x":1}"#;

    // Compute matching signature (testkit emits "sha256=<hex>") and verify.
    let headers = headers_with("X-Signature", &sign_hmac(b"s3cret", body));
    v.verify(&headers, body).expect("valid signature");

    // Bad signature.
    let headers = headers_with("X-Signature", "sha256=00");
    let err = v.verify(&headers, body).expect_err("tampered");
    assert!(err.is_signature_mismatch());
    assert_eq!(err.to_string(), "firefly/webhooks: signature mismatch");
}

#[test]
fn hmac_validator_accepts_unprefixed_hex() {
    let v = HmacValidator::new("generic", b"s3cret");
    let body = b"payload";
    let sig = sign_hmac(b"s3cret", body);
    let bare = sig.strip_prefix("sha256=").expect("prefix");
    let headers = headers_with("X-Signature", bare);
    v.verify(&headers, body).expect("unprefixed hex accepted");
}

#[test]
fn hmac_validator_rejects_missing_header() {
    let v = HmacValidator::new("generic", b"s3cret");
    let err = v.verify(&HeaderMap::new(), b"body").expect_err("missing");
    assert!(err.is_signature_mismatch());
}

#[test]
fn hmac_validator_honours_custom_header_and_base64_mode() {
    use base64::engine::general_purpose::STANDARD as BASE64_STD;
    use base64::Engine as _;
    use hmac::{Hmac, Mac};

    let mut v = HmacValidator::new("generic", b"s3cret");
    v.header = "X-Custom-Sig".to_owned();
    v.hex_encoded = false;

    let body = b"payload";
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"s3cret").expect("hmac key");
    mac.update(body);
    let sig = BASE64_STD.encode(mac.finalize().into_bytes());

    let headers = headers_with("X-Custom-Sig", &sig);
    v.verify(&headers, body).expect("base64 mode accepted");

    // The default header is no longer consulted.
    let headers = headers_with("X-Signature", &sig);
    assert!(v.verify(&headers, body).is_err());
}

#[test]
fn hmac_validator_provider_is_configurable() {
    assert_eq!(HmacValidator::new("acme", b"k").provider(), "acme");
}

// --- Stripe ------------------------------------------------------------------

const STRIPE_TS: i64 = 1_700_000_000;

fn stripe_clock() -> Arc<FixedClock> {
    Arc::new(FixedClock(
        DateTime::from_timestamp(STRIPE_TS, 0).expect("timestamp"),
    ))
}

#[test]
fn stripe_validator_accepts_testkit_signature() {
    let v = StripeValidator::new(b"whsec_test").with_clock(stripe_clock());
    let body = br#"{"type":"charge.succeeded"}"#;
    let headers = headers_with(
        "Stripe-Signature",
        &sign_stripe(b"whsec_test", body, STRIPE_TS),
    );
    v.verify(&headers, body).expect("testkit stripe signature");
    assert_eq!(v.provider(), "stripe");
}

#[test]
fn stripe_validator_rejects_tampered_body() {
    let v = StripeValidator::new(b"whsec_test").with_clock(stripe_clock());
    let headers = headers_with(
        "Stripe-Signature",
        &sign_stripe(b"whsec_test", b"original", STRIPE_TS),
    );
    let err = v.verify(&headers, b"tampered").expect_err("tampered body");
    assert_eq!(err.to_string(), "firefly/webhooks: signature mismatch");
}

#[test]
fn stripe_validator_rejects_stale_timestamp() {
    // The clock sits 6 minutes after the signing instant — outside the
    // canonical 5-minute tolerance.
    let clock = Arc::new(FixedClock(
        DateTime::from_timestamp(STRIPE_TS + 360, 0).expect("timestamp"),
    ));
    let v = StripeValidator::new(b"whsec_test").with_clock(clock);
    let body = b"body";
    let headers = headers_with(
        "Stripe-Signature",
        &sign_stripe(b"whsec_test", body, STRIPE_TS),
    );
    let err = v.verify(&headers, body).expect_err("stale");
    assert!(err.is_signature_mismatch());
    assert_eq!(
        err.to_string(),
        "firefly/webhooks: signature mismatch: stale"
    );
}

#[test]
fn stripe_validator_zero_tolerance_disables_freshness_check() {
    let clock = Arc::new(FixedClock(
        DateTime::from_timestamp(STRIPE_TS + 3_600, 0).expect("timestamp"),
    ));
    let v = StripeValidator::new(b"whsec_test")
        .with_clock(clock)
        .with_tolerance(Duration::ZERO);
    let body = b"body";
    let headers = headers_with(
        "Stripe-Signature",
        &sign_stripe(b"whsec_test", body, STRIPE_TS),
    );
    v.verify(&headers, body)
        .expect("tolerance 0 skips freshness, as in Go");
}

#[test]
fn stripe_validator_accepts_any_matching_v1() {
    let v = StripeValidator::new(b"whsec_test").with_clock(stripe_clock());
    let body = b"body";
    let signed = sign_stripe(b"whsec_test", body, STRIPE_TS);
    let good_v1 = signed.split("v1=").nth(1).expect("v1 part");
    let headers = headers_with(
        "Stripe-Signature",
        &format!("t={STRIPE_TS},v1=deadbeef,v1={good_v1}"),
    );
    v.verify(&headers, body).expect("second v1 matches");
}

#[test]
fn stripe_validator_requires_timestamp_and_signature() {
    let v = StripeValidator::new(b"whsec_test").with_clock(stripe_clock());
    for header in [
        "",
        "v1=abc",
        &format!("t={STRIPE_TS}"),
        "t=notanumber,v1=abc",
    ] {
        let headers = if header.is_empty() {
            HeaderMap::new()
        } else {
            headers_with("Stripe-Signature", header)
        };
        let err = v.verify(&headers, b"body").expect_err("incomplete header");
        assert!(err.is_signature_mismatch(), "header {header:?}: {err}");
    }
}

// --- GitHub ------------------------------------------------------------------

#[test]
fn github_validator_accepts_testkit_signature() {
    let v = GitHubValidator::new(b"gh_secret");
    let body = br#"{"action":"opened"}"#;
    let headers = headers_with("X-Hub-Signature-256", &sign_github(b"gh_secret", body));
    v.verify(&headers, body).expect("testkit github signature");
    assert_eq!(v.provider(), "github");
}

#[test]
fn github_validator_rejects_wrong_secret_and_missing_header() {
    let v = GitHubValidator::new(b"gh_secret");
    let body = br#"{"action":"opened"}"#;

    let headers = headers_with("X-Hub-Signature-256", &sign_github(b"other", body));
    assert!(v.verify(&headers, body).is_err());

    let err = v.verify(&HeaderMap::new(), body).expect_err("missing");
    assert_eq!(err.to_string(), "firefly/webhooks: signature mismatch");
}

// --- Twilio ------------------------------------------------------------------

const TWILIO_URL: &str = "https://example.com/cb";

/// Twilio request headers: the signature plus the form Content-Type
/// real Twilio traffic carries (and Go's `ParseForm` requires before
/// it parses the body).
fn twilio_headers(sig: &str) -> HeaderMap {
    let mut headers = headers_with("X-Twilio-Signature", sig);
    headers.insert(
        http::header::CONTENT_TYPE,
        "application/x-www-form-urlencoded".parse().expect("ct"),
    );
    headers
}

#[test]
fn twilio_validator_accepts_testkit_signature() {
    let v = TwilioValidator::new(b"tok", TWILIO_URL);
    // The urlencoded body for form {From: "+1", Body: "hi"}.
    let body = b"From=%2B1&Body=hi";
    let sig = sign_twilio(b"tok", TWILIO_URL, &[("From", "+1"), ("Body", "hi")]);
    let headers = twilio_headers(&sig);
    v.verify(&headers, body).expect("testkit twilio signature");
    assert_eq!(v.provider(), "twilio");
}

#[test]
fn twilio_validator_rejects_wrong_token_url_or_form() {
    let body = b"From=%2B1&Body=hi";
    let sig = sign_twilio(b"tok", TWILIO_URL, &[("From", "+1"), ("Body", "hi")]);
    let headers = twilio_headers(&sig);

    // Wrong auth token.
    let v = TwilioValidator::new(b"other", TWILIO_URL);
    assert!(v.verify(&headers, body).is_err());

    // Wrong configured URL.
    let v = TwilioValidator::new(b"tok", "https://example.com/other");
    assert!(v.verify(&headers, body).is_err());

    // Tampered form value.
    let v = TwilioValidator::new(b"tok", TWILIO_URL);
    assert!(v.verify(&headers, b"From=%2B2&Body=hi").is_err());

    // Missing header.
    let err = v.verify(&HeaderMap::new(), body).expect_err("missing");
    assert_eq!(err.to_string(), "firefly/webhooks: signature mismatch");

    // Malformed urlencoded body (Go's ParseForm error path).
    assert!(v.verify(&headers, b"From=%zz").is_err());
}

#[test]
fn twilio_validator_signs_url_only_for_non_form_content_types() {
    // Go's ParseForm leaves PostForm empty unless the Content-Type
    // media type is application/x-www-form-urlencoded, so for e.g. a
    // JSON body Go signs the URL alone — the body bytes never
    // participate. Regression test: the Rust port used to fold the raw
    // body into the signed string regardless of Content-Type.
    let v = TwilioValidator::new(b"tok", TWILIO_URL);
    let url_only_sig = sign_twilio(b"tok", TWILIO_URL, &[]);

    // JSON Content-Type: a URL-only signature verifies, as in Go.
    let mut headers = headers_with("X-Twilio-Signature", &url_only_sig);
    headers.insert(
        http::header::CONTENT_TYPE,
        "application/json".parse().expect("ct"),
    );
    v.verify(&headers, br#"{"a":1}"#)
        .expect("Go signs the URL alone for a JSON body");

    // Missing Content-Type counts as application/octet-stream (RFC
    // 7231 §3.1.1.5) — also URL-only, even for form-shaped bytes.
    let headers = headers_with("X-Twilio-Signature", &url_only_sig);
    v.verify(&headers, b"From=%2B1")
        .expect("a body without a Content-Type is not a form");

    // The old (buggy) signed string — URL + raw body folded in as a
    // bare form key — must no longer verify.
    let folded_sig = sign_twilio(b"tok", TWILIO_URL, &[(r#"{"a":1}"#, "")]);
    let mut headers = headers_with("X-Twilio-Signature", &folded_sig);
    headers.insert(
        http::header::CONTENT_TYPE,
        "application/json".parse().expect("ct"),
    );
    assert!(
        v.verify(&headers, br#"{"a":1}"#).is_err(),
        "non-form body bytes must not fold into the signed string"
    );
}

#[test]
fn twilio_validator_follows_go_content_type_parsing() {
    let v = TwilioValidator::new(b"tok", TWILIO_URL);
    let body = b"From=%2B1";
    let form_sig = sign_twilio(b"tok", TWILIO_URL, &[("From", "+1")]);

    // Media-type casing and parameters are normalized away, as in
    // Go's mime.ParseMediaType.
    let mut headers = headers_with("X-Twilio-Signature", &form_sig);
    headers.insert(
        http::header::CONTENT_TYPE,
        "Application/X-WWW-Form-URLencoded; charset=UTF-8"
            .parse()
            .expect("ct"),
    );
    v.verify(&headers, body)
        .expect("casing and media-type parameters are ignored");

    // A Content-Type mime.ParseMediaType rejects makes Go's ParseForm
    // return an error → signature mismatch, whatever the signature.
    for bad in [
        "application/",
        "form/urlencoded/extra",
        "application/x-www-form-urlencoded; charset",
    ] {
        let mut headers = headers_with("X-Twilio-Signature", &form_sig);
        headers.insert(http::header::CONTENT_TYPE, bad.parse().expect("ct"));
        assert!(v.verify(&headers, body).is_err(), "Content-Type {bad:?}");
    }
}

// --- Inbound wire shape --------------------------------------------------------

#[test]
fn inbound_json_matches_go_wire_shape() {
    let ev = Inbound {
        id: "evt-1".to_owned(),
        provider: "stripe".to_owned(),
        event_type: "charge.succeeded".to_owned(),
        headers: BTreeMap::from([("Content-Type".to_owned(), "application/json".to_owned())]),
        payload: br#"{"x":1}"#.to_vec(),
        received_at: DateTime::from_timestamp(STRIPE_TS, 0).expect("timestamp"),
    };
    let json = serde_json::to_string(&ev).expect("serialize");
    // Field order, camelCase names, base64 payload, and RFC 3339 time
    // are byte-identical to Go's encoding/json output.
    assert_eq!(
        json,
        r#"{"id":"evt-1","provider":"stripe","eventType":"charge.succeeded","headers":{"Content-Type":"application/json"},"payload":"eyJ4IjoxfQ==","receivedAt":"2023-11-14T22:13:20Z"}"#
    );

    let back: Inbound = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, ev);
}

// --- Error family ----------------------------------------------------------

#[test]
fn error_display_matches_go_sentinels() {
    assert_eq!(
        WebhookError::SignatureMismatch.to_string(),
        "firefly/webhooks: signature mismatch"
    );
    assert_eq!(
        WebhookError::StaleSignature.to_string(),
        "firefly/webhooks: signature mismatch: stale"
    );
    // Processor errors render their own message, like Go's errors.New.
    assert_eq!(WebhookError::processor("boom").to_string(), "boom");

    assert!(WebhookError::SignatureMismatch.is_signature_mismatch());
    assert!(WebhookError::StaleSignature.is_signature_mismatch());
    assert!(!WebhookError::processor("boom").is_signature_mismatch());
}

// --- Rust-specific bounds ----------------------------------------------------

#[test]
fn public_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<firefly_webhooks::Pipeline>();
    assert_send_sync::<firefly_webhooks::MemoryDlq>();
    assert_send_sync::<firefly_webhooks::Client>();
    assert_send_sync::<HmacValidator>();
    assert_send_sync::<StripeValidator>();
    assert_send_sync::<GitHubValidator>();
    assert_send_sync::<TwilioValidator>();
    assert_send_sync::<Inbound>();
    assert_send_sync::<WebhookError>();
}
