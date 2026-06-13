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

//! The HMAC-signing dispatcher — the Rust spelling of the Go
//! `callbacks/core` sub-package.
//!
//! [`HmacDispatcher`] delivers a [`CallbackEvent`] to every active,
//! type-matching [`Target`] over plain HTTP (`reqwest`, the analog of
//! the raw `*http.Client` the Go core uses), signing each payload with
//! HMAC-SHA256 (`X-Firefly-Signature: sha256=<hmac-hex>`), retrying
//! with exponential backoff, and recording every attempt to the
//! [`Store`] for audit.

use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use http::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use sha2::Sha256;

use async_trait::async_trait;
use firefly_kernel::{Clock, SystemClock, HEADER_CORRELATION_ID};

use crate::interfaces::{
    Attempt, AuthorizedDomain, CallbackError, CallbackEvent, Dispatcher, Store, Target,
};

/// Header carrying the event type (`CallbackEvent::event_type`).
pub const HEADER_EVENT: &str = "X-Firefly-Event";
/// Header carrying the event id (`CallbackEvent::id`).
pub const HEADER_EVENT_ID: &str = "X-Firefly-Event-Id";
/// Header carrying the Unix-seconds send timestamp.
pub const HEADER_TIMESTAMP: &str = "X-Firefly-Timestamp";
/// Header carrying the `sha256=<hmac-hex>` payload signature.
pub const HEADER_SIGNATURE: &str = "X-Firefly-Signature";

/// Default per-request timeout (10 s), matching the Go port.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default attempt budget (3), matching the Go port.
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// Default first-retry delay (200 ms, doubling), matching the Go port.
const DEFAULT_INITIAL_DELAY: Duration = Duration::from_millis(200);

/// Tunes [`HmacDispatcher`] — the Rust spelling of Go's `core.Config`.
///
/// Fields left at their zero value (`None` / `0` / zero duration) are
/// filled with the defaults by [`HmacDispatcher::new`], exactly like
/// Go's `NewDispatcher`.
#[derive(Clone, Default)]
pub struct DispatcherConfig {
    /// HTTP client used for deliveries; defaults to a `reqwest::Client`
    /// with a 10 s timeout (Go's `&http.Client{Timeout: 10s}`).
    pub http_client: Option<reqwest::Client>,
    /// Total attempts per target (default 3).
    pub max_attempts: u32,
    /// First retry delay (default 200 ms), doubling per attempt.
    pub initial_delay: Duration,
    /// Time source for `X-Firefly-Timestamp` and attempt audit rows;
    /// defaults to [`SystemClock`] (Go's `time.Now`).
    pub clock: Option<Arc<dyn Clock>>,
    /// Outbound-URL allowlist (SSRF protection) — the Rust spelling of
    /// pyfly's `CallbackConfig.authorized_domains`.
    ///
    /// When **non-empty**, [`HmacDispatcher::dispatch`] delivers to a
    /// target only if its URL host equals (or is a subdomain of) one of
    /// these domains; a target failing the check is recorded as a
    /// rejected attempt and skipped — no HTTP request is made (pyfly
    /// audit #190). An **empty** list disables the allowlist: every
    /// target is reachable, preserving the prior (Go-parity) behaviour.
    pub authorized_domains: Vec<AuthorizedDomain>,
}

impl std::fmt::Debug for DispatcherConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatcherConfig")
            .field("http_client", &self.http_client)
            .field("max_attempts", &self.max_attempts)
            .field("initial_delay", &self.initial_delay)
            .field("clock", &self.clock.as_ref().map(|_| "Arc<dyn Clock>"))
            .field("authorized_domains", &self.authorized_domains)
            .finish()
    }
}

/// The canonical [`Dispatcher`] implementation — Go's
/// `core.Dispatcher`, renamed so the struct and the port trait can both
/// be re-exported flat from the crate root.
pub struct HmacDispatcher {
    store: Arc<dyn Store>,
    http: reqwest::Client,
    max_attempts: u32,
    initial_delay: Duration,
    clock: Arc<dyn Clock>,
    authorized_domains: Vec<AuthorizedDomain>,
}

impl HmacDispatcher {
    /// Returns a dispatcher using `store` + `cfg`. Any field on `cfg`
    /// left at its zero value is filled with the default (10 s-timeout
    /// client, 3 attempts, 200 ms initial delay, system clock) — the
    /// contract of Go's `NewDispatcher(store, cfg)`.
    pub fn new(store: Arc<dyn Store>, cfg: DispatcherConfig) -> Self {
        let http = cfg.http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(DEFAULT_TIMEOUT)
                .build()
                .expect("HmacDispatcher::new: reqwest client construction failed")
        });
        Self {
            store,
            http,
            max_attempts: if cfg.max_attempts == 0 {
                DEFAULT_MAX_ATTEMPTS
            } else {
                cfg.max_attempts
            },
            initial_delay: if cfg.initial_delay.is_zero() {
                DEFAULT_INITIAL_DELAY
            } else {
                cfg.initial_delay
            },
            clock: cfg.clock.unwrap_or_else(|| Arc::new(SystemClock)),
            authorized_domains: cfg.authorized_domains,
        }
    }

    /// Delivers `ev` to one target: up to `max_attempts` tries with the
    /// delay doubling between them, recording an [`Attempt`] audit row
    /// per try regardless of outcome.
    async fn deliver(&self, target: &Target, ev: &CallbackEvent) -> Result<(), CallbackError> {
        let mut delay = self.initial_delay;
        for attempt in 1..=self.max_attempts {
            let started = self.clock.now();
            let (status, body, error) = self.send(target, ev).await;
            let finished = self.clock.now();
            // Best-effort audit, as in the Go port.
            let _ = self
                .store
                .record_attempt(Attempt {
                    id: new_id(),
                    event_id: ev.id.clone(),
                    target_id: target.id.clone(),
                    status,
                    body,
                    error: error.clone().unwrap_or_default(),
                    attempt,
                    started_at: started,
                    finished_at: finished,
                })
                .await;
            if error.is_none() && (200..300).contains(&status) {
                return Ok(());
            }
            if attempt == self.max_attempts {
                return Err(CallbackError::DeliveryFailed { status, error });
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        Ok(())
    }

    /// Records a rejected-delivery audit row for a target blocked by the
    /// [`authorized_domains`](DispatcherConfig::authorized_domains)
    /// allowlist. No HTTP request is made: the row carries status `0`,
    /// the explanatory error, and `attempt: 0` (no delivery was
    /// attempted) — the audit-trail analog of pyfly's failed execution
    /// with `last_error = "Domain not authorized"`.
    async fn record_rejection(&self, target: &Target, ev: &CallbackEvent) {
        let now = self.clock.now();
        let _ = self
            .store
            .record_attempt(Attempt {
                id: new_id(),
                event_id: ev.id.clone(),
                target_id: target.id.clone(),
                status: 0,
                body: String::new(),
                error: ERR_DOMAIN_NOT_AUTHORIZED.to_owned(),
                attempt: 0,
                started_at: now,
                finished_at: now,
            })
            .await;
    }

    /// Sends one signed POST to the target. Returns
    /// `(status, response body, transport error)` — status `0` and an
    /// error message when the request never produced a response, the
    /// shape of Go's `send` returning `(int, string, error)`.
    async fn send(&self, target: &Target, ev: &CallbackEvent) -> (u16, String, Option<String>) {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        set_header(&mut headers, HEADER_EVENT, &ev.event_type);
        set_header(&mut headers, HEADER_EVENT_ID, &ev.id);
        set_header(
            &mut headers,
            HEADER_TIMESTAMP,
            &self.clock.now().timestamp().to_string(),
        );
        if !ev.correlation_id.is_empty() {
            set_header(&mut headers, HEADER_CORRELATION_ID, &ev.correlation_id);
        }
        for (k, v) in &target.headers {
            set_header(&mut headers, k, v);
        }
        // Canonicalize the payload to sorted-key JSON *before* both
        // signing and sending, so the wire body and its HMAC always match
        // Go's `encoding/json` sorted-map output byte-for-byte — and so
        // the contract holds regardless of how the caller serialized the
        // payload or whether the `serde_json/preserve_order` feature is
        // active anywhere in the workspace graph. Non-JSON payloads pass
        // through unchanged.
        let body = canonical_payload(&ev.payload);
        // After the custom headers, so the signature always wins —
        // identical ordering to the Go port.
        if !target.secret.is_empty() {
            set_header(
                &mut headers,
                HEADER_SIGNATURE,
                &sign(target.secret.as_bytes(), &body),
            );
        }

        let result = self
            .http
            .post(&target.url)
            .headers(headers)
            .body(body)
            .send()
            .await;
        match result {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // Body read errors are ignored, as in the Go port.
                let body = resp.text().await.unwrap_or_default();
                (status, body, None)
            }
            Err(err) => (0, String::new(), Some(err.to_string())),
        }
    }
}

#[async_trait]
impl Dispatcher for HmacDispatcher {
    /// The event is delivered to every active target whose
    /// `event_types` match (empty = match-all). Each delivery records
    /// an [`Attempt`] audit entry; per-target failures are best-effort
    /// (recorded, then the next target is tried), exactly as in Go.
    ///
    /// When the dispatcher has a non-empty
    /// [`authorized_domains`](DispatcherConfig::authorized_domains)
    /// allowlist (SSRF protection), a matching target whose URL host is
    /// not authorized is **rejected before any HTTP request** — a
    /// rejected-attempt audit row is recorded and the target is skipped
    /// (pyfly audit #190).
    async fn dispatch(&self, event: CallbackEvent) -> Result<(), CallbackError> {
        let targets = self.store.list_targets().await?;
        for target in targets {
            if !target.active || !matches_type(&target, &event.event_type) {
                continue;
            }
            if !self.authorized_domains.is_empty()
                && !is_authorized(&target.url, &self.authorized_domains)
            {
                tracing::debug!(target = %target.id, event = %event.id, url = %target.url, "callback target rejected: domain not authorized");
                self.record_rejection(&target, &event).await;
                continue;
            }
            if let Err(err) = self.deliver(&target, &event).await {
                tracing::debug!(target = %target.id, event = %event.id, error = %err, "callback delivery failed");
            }
        }
        Ok(())
    }
}

/// The audit-row / log message recorded when an outbound target is
/// blocked by the [`authorized_domains`](DispatcherConfig::authorized_domains)
/// allowlist — pyfly's `"Domain not authorized"`, lowercased to match
/// the framework's snake-case error-message convention.
const ERR_DOMAIN_NOT_AUTHORIZED: &str = "domain not authorized";

/// Reports whether the target subscribes to `event_type` (an empty
/// subscription list matches every type).
fn matches_type(target: &Target, event_type: &str) -> bool {
    target.event_types.is_empty() || target.event_types.iter().any(|et| et == event_type)
}

/// Reports whether `target_url`'s host is allowed by `domains` — the
/// Rust spelling of pyfly's `_is_authorized`.
///
/// The host is extracted from the URL (see [`host_of`]) and lowercased;
/// a URL with no parseable host is **never** authorized (fail-closed). A
/// host is authorized when it equals an entry's domain or is a subdomain
/// of it (`host == domain` or `host` ends with `".{domain}"`), matched
/// case-insensitively after trimming each entry.
fn is_authorized(target_url: &str, domains: &[AuthorizedDomain]) -> bool {
    let Some(host) = host_of(target_url) else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    domains.iter().any(|entry| {
        let allowed = entry.domain.trim().to_ascii_lowercase();
        !allowed.is_empty() && (host == allowed || host.ends_with(&format!(".{allowed}")))
    })
}

/// Extracts the lowercase host from an absolute URL — the dependency-free
/// analog of pyfly's `urlparse(target_url).hostname`.
///
/// Strips the `scheme://`, any `userinfo@` prefix, then the `:port`,
/// path, query, and fragment, returning what remains (which may be an
/// IPv4 literal or a bracketed IPv6 literal). Returns `None` when the
/// input has no `://` authority component.
fn host_of(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    // Authority ends at the first '/', '?', or '#'.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Drop any userinfo ("user:pass@host").
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // Bracketed IPv6 literal: "[::1]:8080" → "[::1]".
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        let end = rest.find(']')?;
        &host_port[..end + 2]
    } else {
        // Strip the ":port" suffix from a host:port pair.
        host_port.split(':').next().unwrap_or(host_port)
    };
    Some(host.to_ascii_lowercase())
}

/// Returns the canonical (sorted-key) JSON encoding of `payload`.
///
/// If `payload` parses as JSON, it is re-emitted with every object's
/// keys in sorted order — byte-for-byte identical to Go's
/// `encoding/json` map encoding, and independent of the
/// `serde_json/preserve_order` feature. If `payload` is not valid JSON
/// it is returned unchanged, so opaque (non-JSON) bodies still flow
/// through untouched.
fn canonical_payload(payload: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<serde_json::Value>(payload) {
        Ok(value) => {
            serde_json::to_vec(&canonicalize_value(value)).unwrap_or_else(|_| payload.to_vec())
        }
        Err(_) => payload.to_vec(),
    }
}

/// Recursively rebuilds `value` so every object's keys are inserted in
/// sorted order. Because the keys are *inserted* sorted, the resulting
/// [`serde_json::Map`] serializes sorted regardless of whether
/// `preserve_order` (insertion-order) is enabled.
fn canonicalize_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: std::collections::BTreeMap<String, serde_json::Value> =
                std::collections::BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k, canonicalize_value(v));
            }
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonicalize_value).collect())
        }
        other => other,
    }
}

/// Computes the `X-Firefly-Signature` value:
/// `sha256=` + lowercase hex of HMAC-SHA256(`secret`, `payload`) —
/// byte-for-byte the Go port's `sign`.
fn sign(secret: &[u8], payload: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).expect("HMAC-SHA256 accepts any key length");
    mac.update(payload);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Random attempt id: 12 random bytes hex-encoded (24 lowercase hex
/// chars), the format of the Go port's `newID`.
fn new_id() -> String {
    hex::encode(&uuid::Uuid::new_v4().as_bytes()[..12])
}

/// Sets a header with Go `http.Header.Set` semantics (replace, not
/// append). Names or values that are not valid HTTP are skipped.
fn set_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_matches_rfc_test_vector() {
        // RFC 4231-adjacent known-answer test:
        // HMAC-SHA256("key", "The quick brown fox jumps over the lazy dog")
        let got = sign(b"key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            got,
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn matches_type_empty_list_matches_all() {
        let t = Target::default();
        assert!(matches_type(&t, "anything"));
        let t = Target {
            event_types: vec!["order.placed".into(), "order.shipped".into()],
            ..Target::default()
        };
        assert!(matches_type(&t, "order.placed"));
        assert!(!matches_type(&t, "order.cancelled"));
    }

    #[test]
    fn host_of_extracts_lowercase_host_from_urls() {
        assert_eq!(
            host_of("https://Example.com/cb").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            host_of("http://customer.example.com:8443/hook?x=1#frag").as_deref(),
            Some("customer.example.com")
        );
        assert_eq!(
            host_of("https://user:pass@host.example.org/p").as_deref(),
            Some("host.example.org")
        );
        assert_eq!(
            host_of("https://10.0.0.1:9000/x").as_deref(),
            Some("10.0.0.1")
        );
        // Bracketed IPv6 literal keeps its brackets, drops the port.
        assert_eq!(host_of("http://[::1]:8080/x").as_deref(), Some("[::1]"));
        // No authority component → no host.
        assert_eq!(host_of("not-a-url"), None);
        assert_eq!(host_of("mailto:user@example.com"), None);
    }

    #[test]
    fn is_authorized_matches_exact_and_subdomains_case_insensitively() {
        let allow = vec![AuthorizedDomain::new("trusted.example.com")];
        // Exact host match (case-insensitive).
        assert!(is_authorized("https://trusted.example.com/x", &allow));
        assert!(is_authorized("https://TRUSTED.example.com/x", &allow));
        // Subdomain match.
        assert!(is_authorized("https://api.trusted.example.com/x", &allow));
        // A different host is rejected.
        assert!(!is_authorized("https://evil.example.org/x", &allow));
        // A "suffix but not subdomain" host must NOT match (no leading dot).
        assert!(!is_authorized("https://nottrusted.example.com/x", &allow));
        // Unparseable URL → fail-closed.
        assert!(!is_authorized("garbage", &allow));
    }

    #[test]
    fn is_authorized_trims_entries_and_ignores_blanks() {
        let allow = vec![
            AuthorizedDomain::new("  trusted.example.com  "),
            AuthorizedDomain::new("   "),
        ];
        assert!(is_authorized("https://trusted.example.com/x", &allow));
        // A blank entry never authorizes anything.
        assert!(!is_authorized("https://", &allow));
    }

    #[test]
    fn new_id_is_24_lowercase_hex_chars() {
        let id = new_id();
        assert_eq!(id.len(), 24);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(new_id(), id);
    }

    #[test]
    fn zero_config_fields_fall_back_to_defaults() {
        let store = Arc::new(crate::models::MemoryStore::new());
        let d = HmacDispatcher::new(store, DispatcherConfig::default());
        assert_eq!(d.max_attempts, DEFAULT_MAX_ATTEMPTS);
        assert_eq!(d.initial_delay, DEFAULT_INITIAL_DELAY);

        let store = Arc::new(crate::models::MemoryStore::new());
        let d = HmacDispatcher::new(
            store,
            DispatcherConfig {
                max_attempts: 5,
                initial_delay: Duration::from_millis(1),
                ..DispatcherConfig::default()
            },
        );
        assert_eq!(d.max_attempts, 5);
        assert_eq!(d.initial_delay, Duration::from_millis(1));
    }
}
