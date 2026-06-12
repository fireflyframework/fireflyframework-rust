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

//! The four canonical signature validators — generic HMAC, Stripe,
//! GitHub, and Twilio — ported 1:1 from Go's
//! `webhooks/core/validators.go`. All comparisons are constant-time
//! (Go's `hmac.Equal`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;

use firefly_kernel::{Clock, SystemClock};

use crate::core::sha1::hmac_sha1;
use crate::core::util::{compute_hmac_base64, compute_hmac_hex, ct_eq};
use crate::error::WebhookError;
use crate::interfaces::Validator;

/// Returns the first value of `name` as a string, or `""` when the
/// header is missing or not valid UTF-8 — Go's `req.Header.Get`.
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// The generic HMAC-SHA256 hex-encoded validator. It reads the
/// signature from a configurable header (default `X-Signature`) and
/// accepts an optional `sha256=` prefix.
///
/// # Example
///
/// ```
/// use firefly_webhooks::{HmacValidator, Validator};
/// use http::HeaderMap;
///
/// let v = HmacValidator::new("generic", b"s3cret");
/// assert_eq!(v.provider(), "generic");
///
/// let mut headers = HeaderMap::new();
/// headers.insert("X-Signature", "sha256=00".parse().unwrap());
/// assert!(v.verify(&headers, br#"{"x":1}"#).is_err());
/// ```
#[derive(Debug, Clone)]
pub struct HmacValidator {
    /// The provider key this validator serves.
    pub provider_name: String,
    /// The shared HMAC secret.
    pub secret: Vec<u8>,
    /// The header carrying the signature (default `X-Signature`).
    pub header: String,
    /// `true` (default) compares against the hex encoding; `false`
    /// against standard base64.
    pub hex_encoded: bool,
}

impl HmacValidator {
    /// Returns a generic HMAC validator for `provider` with the
    /// canonical defaults (`X-Signature` header, hex encoding).
    pub fn new(provider: impl Into<String>, secret: impl Into<Vec<u8>>) -> Self {
        Self {
            provider_name: provider.into(),
            secret: secret.into(),
            header: "X-Signature".to_owned(),
            hex_encoded: true,
        }
    }
}

impl Validator for HmacValidator {
    fn provider(&self) -> &str {
        &self.provider_name
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), WebhookError> {
        let raw = header_str(headers, &self.header);
        let got = raw.strip_prefix("sha256=").unwrap_or(raw);
        if got.is_empty() {
            return Err(WebhookError::SignatureMismatch);
        }
        let want = if self.hex_encoded {
            compute_hmac_hex(&self.secret, body)
        } else {
            compute_hmac_base64(&self.secret, body)
        };
        if !ct_eq(got.as_bytes(), want.as_bytes()) {
            return Err(WebhookError::SignatureMismatch);
        }
        Ok(())
    }
}

/// Stripe's `t=<unix>,v1=<hmac-hex>` signature scheme over the
/// `Stripe-Signature` header, with a freshness tolerance (canonically
/// five minutes). The signed payload is `<unix>.<body>`.
#[derive(Clone)]
pub struct StripeValidator {
    /// The endpoint's signing secret (`whsec_…`).
    pub secret: Vec<u8>,
    /// Maximum allowed `|now − t|` skew; `Duration::ZERO` disables the
    /// freshness check. Default: five minutes.
    pub tolerance: Duration,
    clock: Arc<dyn Clock>,
}

impl StripeValidator {
    /// Returns a Stripe validator with the canonical 5-minute
    /// tolerance and the system clock.
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
            tolerance: Duration::from_secs(5 * 60),
            clock: Arc::new(SystemClock),
        }
    }

    /// Substitutes the time source — the analog of overriding Go's
    /// exported `Now` field in tests.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Overrides the freshness tolerance.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }
}

impl std::fmt::Debug for StripeValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripeValidator")
            .field("tolerance", &self.tolerance)
            .finish_non_exhaustive()
    }
}

impl Validator for StripeValidator {
    fn provider(&self) -> &str {
        "stripe"
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), WebhookError> {
        let header = header_str(headers, "Stripe-Signature");
        if header.is_empty() {
            return Err(WebhookError::SignatureMismatch);
        }
        let mut ts: i64 = 0;
        let mut sigs: Vec<&str> = Vec::new();
        for part in header.split(',') {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            match k {
                "t" => ts = v.parse().unwrap_or(0),
                "v1" => sigs.push(v),
                _ => {}
            }
        }
        if ts == 0 || sigs.is_empty() {
            return Err(WebhookError::SignatureMismatch);
        }
        if !self.tolerance.is_zero()
            && (self.clock.now().timestamp() - ts).abs() > self.tolerance.as_secs() as i64
        {
            return Err(WebhookError::StaleSignature);
        }
        let mut signed = ts.to_string().into_bytes();
        signed.push(b'.');
        signed.extend_from_slice(body);
        let want = compute_hmac_hex(&self.secret, &signed);
        if sigs
            .iter()
            .any(|sig| ct_eq(sig.as_bytes(), want.as_bytes()))
        {
            return Ok(());
        }
        Err(WebhookError::SignatureMismatch)
    }
}

/// GitHub's `X-Hub-Signature-256: sha256=<hmac-hex>` scheme.
#[derive(Debug, Clone)]
pub struct GitHubValidator {
    /// The webhook's shared secret.
    pub secret: Vec<u8>,
}

impl GitHubValidator {
    /// Returns a GitHub validator.
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
        }
    }
}

impl Validator for GitHubValidator {
    fn provider(&self) -> &str {
        "github"
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), WebhookError> {
        let raw = header_str(headers, "X-Hub-Signature-256");
        let got = raw.strip_prefix("sha256=").unwrap_or(raw);
        if got.is_empty() {
            return Err(WebhookError::SignatureMismatch);
        }
        let want = compute_hmac_hex(&self.secret, body);
        if !ct_eq(got.as_bytes(), want.as_bytes()) {
            return Err(WebhookError::SignatureMismatch);
        }
        Ok(())
    }
}

/// Twilio's `X-Twilio-Signature` scheme: HMAC-SHA1 of
/// `URL + sorted(form key+value)`, base64-encoded.
///
/// Form parameters participate only when the request's `Content-Type`
/// media type is `application/x-www-form-urlencoded`, mirroring Go's
/// `Request.ParseForm` on the POSTs the ingestion endpoint admits: for
/// any other body — JSON, a missing `Content-Type` (treated as
/// `application/octet-stream`, RFC 7231 §3.1.1.5), multipart — the
/// signed string degenerates to the URL alone, exactly like Go's empty
/// `PostForm`. A malformed `Content-Type` or form body is a signature
/// mismatch (Go's `ParseForm` error path). Only the first value of a
/// repeated key participates (Go's `url.Values.Get`).
#[derive(Debug, Clone)]
pub struct TwilioValidator {
    /// The account's auth token (the HMAC key).
    pub auth_token: Vec<u8>,
    /// The public endpoint URL Twilio is hitting, including scheme and
    /// query string — it prefixes the signed payload.
    pub url: String,
}

impl TwilioValidator {
    /// Returns a Twilio validator. `url` must match the public endpoint
    /// Twilio is hitting (including scheme and query string).
    pub fn new(auth_token: impl Into<Vec<u8>>, url: impl Into<String>) -> Self {
        Self {
            auth_token: auth_token.into(),
            url: url.into(),
        }
    }
}

impl Validator for TwilioValidator {
    fn provider(&self) -> &str {
        "twilio"
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), WebhookError> {
        use base64::engine::general_purpose::STANDARD as BASE64_STD;
        use base64::Engine as _;

        let got = header_str(headers, "X-Twilio-Signature");
        if got.is_empty() {
            return Err(WebhookError::SignatureMismatch);
        }
        let Some(form) = post_form(headers, body) else {
            return Err(WebhookError::SignatureMismatch);
        };
        let mut signed = self.url.clone();
        for (k, v) in &form {
            signed.push_str(k);
            signed.push_str(v);
        }
        let want = BASE64_STD.encode(hmac_sha1(&self.auth_token, signed.as_bytes()));
        if !ct_eq(got.as_bytes(), want.as_bytes()) {
            return Err(WebhookError::SignatureMismatch);
        }
        Ok(())
    }
}

/// Go's `ParseForm` body cap for non-`MaxBytesReader` bodies:
/// `int64(10 << 20)` — "10 MB is a lot of text."
const MAX_FORM_SIZE: usize = 10 << 20;

/// Mirrors Go's `Request.ParseForm` on the POSTs the ingestion
/// endpoint admits: the body is parsed as a form only when the
/// `Content-Type` media type is `application/x-www-form-urlencoded`
/// (a missing `Content-Type` counts as `application/octet-stream`,
/// RFC 7231 §3.1.1.5). Any other media type yields an *empty*
/// parameter map — Go leaves `PostForm` empty — so the Twilio signed
/// string is the URL alone. Returns `None` where `ParseForm` would
/// error: a `Content-Type` that `mime.ParseMediaType` rejects, a
/// malformed form body, or a form body over Go's 10 MB cap
/// (`http: POST too large`).
fn post_form(headers: &HeaderMap, body: &[u8]) -> Option<BTreeMap<String, String>> {
    let ct = header_str(headers, "Content-Type");
    let ct = if ct.is_empty() {
        "application/octet-stream"
    } else {
        ct
    };
    let media_type = crate::core::mime::parse_media_type(ct)?;
    if media_type != "application/x-www-form-urlencoded" {
        return Some(BTreeMap::new());
    }
    if body.len() > MAX_FORM_SIZE {
        return None;
    }
    parse_form(body)
}

/// Parses an `application/x-www-form-urlencoded` body into a sorted
/// first-value-per-key map, mirroring Go's `Request.ParseForm` +
/// `url.Values.Get`: `+` decodes to space, `%XX` escapes are required
/// to be valid, a `;` separator is an error (Go ≥ 1.17), and repeated
/// keys keep their first value. Returns `None` on any parse error.
fn parse_form(body: &[u8]) -> Option<BTreeMap<String, String>> {
    let s = std::str::from_utf8(body).ok()?;
    let mut out = BTreeMap::new();
    for segment in s.split('&') {
        if segment.contains(';') {
            return None;
        }
        if segment.is_empty() {
            continue;
        }
        let (k, v) = segment.split_once('=').unwrap_or((segment, ""));
        let k = url_decode(k)?;
        let v = url_decode(v)?;
        out.entry(k).or_insert(v);
    }
    Some(out)
}

/// Percent-decodes one form token (`+` → space). Returns `None` on a
/// truncated or non-hex escape, or when the decoded bytes are not
/// valid UTF-8.
fn url_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hi = hex_val(*bytes.get(i + 1)?)?;
                let lo = hex_val(*bytes.get(i + 2)?)?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_form_decodes_sorts_and_keeps_first_values() {
        let form = parse_form(b"From=%2B1&Body=hi+there&From=%2B2").expect("parse");
        let pairs: Vec<_> = form.iter().collect();
        assert_eq!(
            pairs,
            vec![
                (&"Body".to_owned(), &"hi there".to_owned()),
                (&"From".to_owned(), &"+1".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_form_rejects_bad_escape_and_semicolon() {
        assert!(parse_form(b"a=%zz").is_none());
        assert!(parse_form(b"a=%2").is_none());
        assert!(parse_form(b"a=1;b=2").is_none());
    }

    #[test]
    fn parse_form_accepts_empty_segments_and_bare_keys() {
        let form = parse_form(b"&a&b=").expect("parse");
        assert_eq!(form.get("a").map(String::as_str), Some(""));
        assert_eq!(form.get("b").map(String::as_str), Some(""));
    }
}
