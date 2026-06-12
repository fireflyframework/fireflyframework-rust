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

//! DTOs and ports of the callbacks subsystem — the Rust spelling of the
//! Go `callbacks/interfaces` sub-package (plus the error vocabulary the
//! Go port keeps in `callbacks/models/errors.go`).
//!
//! [`Target`], [`CallbackEvent`], and [`Attempt`] serialize with the
//! exact JSON shape of the Go structs (camelCase keys, `omitempty`
//! fields skipped when empty, `Target.secret` never on the wire,
//! `CallbackEvent.payload` base64-encoded like Go's `[]byte`). [`Store`]
//! and [`Dispatcher`] are the persistence and dispatch ports.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Errors produced by the callbacks ports and their default
/// implementations.
///
/// The Go port returns plain `error` values and exposes the
/// `ErrNotFound` sentinel from `callbacks/models`; the Rust port makes
/// the failure classes explicit in one `thiserror` enum whose `Display`
/// output is bytes-equal to the Go strings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallbackError {
    /// The canonical missing-target error. Display output matches the
    /// Go port's `ErrNotFound` (`firefly/callbacks: not found`).
    #[error("firefly/callbacks: not found")]
    NotFound,

    /// Every delivery attempt against one target was consumed without a
    /// 2xx response. Display output matches Go's
    /// `callback delivery failed: status=%d err=%v` (where a `nil`
    /// transport error renders as `<nil>`).
    #[error("callback delivery failed: status={status} err={}", error.as_deref().unwrap_or("<nil>"))]
    DeliveryFailed {
        /// HTTP status of the final attempt (`0` when the request never
        /// produced a response).
        status: u16,
        /// Transport error of the final attempt, when any.
        error: Option<String>,
    },

    /// Any other store-specific failure (database outage, serialization
    /// fault, …). The message is rendered verbatim — the escape hatch
    /// Go's plain `error` return gives custom `Store` implementations.
    #[error("{0}")]
    Store(String),
}

impl CallbackError {
    /// Builds a store-specific [`CallbackError::Store`] from any message.
    pub fn store(message: impl Into<String>) -> Self {
        CallbackError::Store(message.into())
    }

    /// Returns `true` when the error is the canonical
    /// [`CallbackError::NotFound`] sentinel — the analog of Go's
    /// `errors.Is(err, models.ErrNotFound)`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, CallbackError::NotFound)
    }
}

/// The Go/Java zero-time sentinel (`0001-01-01T00:00:00Z`) used as the
/// default for unset timestamps so the JSON wire format matches the Go
/// port's zero `time.Time`.
pub(crate) fn zero_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).unwrap()
}

/// Serde adapter matching Go's `[]byte` JSON encoding: a standard
/// base64 string on the wire, with JSON `null` decoding to empty bytes.
mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            Some(s) => STANDARD.decode(s).map_err(D::Error::custom),
            None => Ok(Vec::new()),
        }
    }
}

/// One entry in a dispatcher's outbound-URL allowlist — the Rust
/// spelling of pyfly's `callbacks.models.AuthorizedDomain`.
///
/// When a dispatcher is configured with one or more authorized domains
/// (an SSRF allowlist), a [`Target`] is delivered to only if its URL
/// host equals an authorized [`domain`](AuthorizedDomain::domain) or is
/// a subdomain of it (`host == domain` or `host` ends with
/// `".{domain}"`), matched case-insensitively — exactly pyfly's
/// `_is_authorized` rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AuthorizedDomain {
    /// The allowlisted host (e.g. `customer.example.com`). Compared
    /// case-insensitively; surrounding whitespace is ignored.
    pub domain: String,
    /// Free-form human description, never used in matching. Omitted from
    /// JSON when empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl AuthorizedDomain {
    /// Builds an [`AuthorizedDomain`] for `domain` with no description —
    /// the common case.
    pub fn new(domain: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            description: String::new(),
        }
    }
}

impl From<&str> for AuthorizedDomain {
    fn from(domain: &str) -> Self {
        AuthorizedDomain::new(domain)
    }
}

impl From<String> for AuthorizedDomain {
    fn from(domain: String) -> Self {
        AuthorizedDomain::new(domain)
    }
}

/// Target is one outbound delivery destination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Target {
    /// Unique target identifier.
    pub id: String,
    /// Absolute URL the dispatcher POSTs callbacks to.
    pub url: String,
    /// Shared HMAC-SHA256 key. Never serialized (Go's `json:"-"`), so
    /// the admin API can neither leak nor accept it over the wire.
    #[serde(skip)]
    pub secret: String,
    /// Event types this target subscribes to; empty = match-all.
    /// Omitted from JSON when empty (Go's `omitempty`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub event_types: Vec<String>,
    /// Extra headers stamped on every delivery to this target.
    /// Omitted from JSON when empty (Go's `omitempty`).
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    /// Inactive targets are skipped by the dispatcher.
    pub active: bool,
    /// Registration timestamp (UTC); the Go zero time when unset.
    pub created_at: DateTime<Utc>,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            id: String::new(),
            url: String::new(),
            secret: String::new(),
            event_types: Vec::new(),
            headers: HashMap::new(),
            active: false,
            created_at: zero_time(),
        }
    }
}

/// CallbackEvent is a single outbound payload bound for one Target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CallbackEvent {
    /// Unique event identifier, echoed as `X-Firefly-Event-Id`.
    pub id: String,
    /// Event type (e.g. `order.placed`), echoed as `X-Firefly-Event`
    /// and matched against [`Target::event_types`]. Serialized as
    /// `"type"`, matching the Go field tag.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Raw request body delivered to the target. Encoded as a standard
    /// base64 string in JSON, matching Go's `[]byte` marshalling.
    #[serde(with = "base64_bytes")]
    pub payload: Vec<u8>,
    /// Free-form metadata carried with the event (not stamped on the
    /// outbound request — same as the Go port). Omitted when empty.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    /// Correlation id forwarded as `X-Correlation-Id` when non-empty.
    /// Omitted from JSON when empty (Go's `omitempty`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub correlation_id: String,
}

/// Attempt records a single delivery attempt against a Target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Attempt {
    /// Unique attempt identifier (24 lowercase hex chars).
    pub id: String,
    /// The [`CallbackEvent::id`] this attempt delivered.
    pub event_id: String,
    /// The [`Target::id`] this attempt delivered to.
    pub target_id: String,
    /// HTTP status of the response (`0` when the request never
    /// produced a response).
    pub status: u16,
    /// Response body, when one was read. Omitted from JSON when empty
    /// (Go's `omitempty`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// Transport error message, when the request failed before a
    /// response. Omitted from JSON when empty (Go's `omitempty`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// 1-based attempt counter within one delivery.
    pub attempt: u32,
    /// When the attempt started (UTC).
    pub started_at: DateTime<Utc>,
    /// When the attempt finished (UTC).
    pub finished_at: DateTime<Utc>,
}

impl Default for Attempt {
    fn default() -> Self {
        Self {
            id: String::new(),
            event_id: String::new(),
            target_id: String::new(),
            status: 0,
            body: String::new(),
            error: String::new(),
            attempt: 0,
            started_at: zero_time(),
            finished_at: zero_time(),
        }
    }
}

/// Store is the persistence boundary for Targets and Attempts.
#[async_trait]
pub trait Store: Send + Sync {
    /// Inserts or replaces the target keyed by [`Target::id`] and
    /// returns the stored value.
    async fn upsert_target(&self, target: Target) -> Result<Target, CallbackError>;

    /// Returns the target with the given id, or
    /// [`CallbackError::NotFound`].
    async fn get_target(&self, id: &str) -> Result<Target, CallbackError>;

    /// Returns every registered target.
    async fn list_targets(&self) -> Result<Vec<Target>, CallbackError>;

    /// Removes the target with the given id, or fails with
    /// [`CallbackError::NotFound`].
    async fn delete_target(&self, id: &str) -> Result<(), CallbackError>;

    /// Appends one delivery-attempt audit row.
    async fn record_attempt(&self, attempt: Attempt) -> Result<(), CallbackError>;

    /// Returns every attempt recorded for the given event id, oldest
    /// first. Empty when the event has no recorded attempts.
    async fn list_attempts(&self, event_id: &str) -> Result<Vec<Attempt>, CallbackError>;
}

/// Dispatcher is the dispatch port — fanning a [`CallbackEvent`] out to
/// every matching [`Target`].
#[async_trait]
pub trait Dispatcher: Send + Sync {
    /// Delivers the event to every active target whose
    /// [`Target::event_types`] match (empty = match-all).
    async fn dispatch(&self, event: CallbackEvent) -> Result<(), CallbackError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_display_matches_go_sentinel() {
        assert_eq!(
            CallbackError::NotFound.to_string(),
            "firefly/callbacks: not found"
        );
        assert!(CallbackError::NotFound.is_not_found());
        assert!(!CallbackError::store("boom").is_not_found());
    }

    #[test]
    fn delivery_failed_display_matches_go_fmt() {
        // Go: fmt.Errorf("callback delivery failed: status=%d err=%v", 500, nil)
        let nil_err = CallbackError::DeliveryFailed {
            status: 500,
            error: None,
        };
        assert_eq!(
            nil_err.to_string(),
            "callback delivery failed: status=500 err=<nil>"
        );
        let transport = CallbackError::DeliveryFailed {
            status: 0,
            error: Some("connection refused".into()),
        };
        assert_eq!(
            transport.to_string(),
            "callback delivery failed: status=0 err=connection refused"
        );
    }

    #[test]
    fn target_json_omits_secret_and_empty_fields() {
        let t = Target {
            id: "t1".into(),
            url: "https://example.com/cb".into(),
            secret: "s3cret".into(),
            active: true,
            ..Target::default()
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["id"], "t1");
        assert_eq!(json["url"], "https://example.com/cb");
        assert_eq!(json["active"], true);
        // Go zero time, rendered exactly as encoding/json renders it.
        assert_eq!(json["createdAt"], "0001-01-01T00:00:00Z");
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("secret"), "secret must never serialize");
        assert!(!obj.contains_key("eventTypes"), "omitempty");
        assert!(!obj.contains_key("headers"), "omitempty");
    }

    #[test]
    fn target_json_ignores_incoming_secret() {
        // Go's `json:"-"` drops the field on decode too.
        let t: Target = serde_json::from_str(
            r#"{"id":"t1","url":"https://example.com","secret":"leaked","active":true}"#,
        )
        .unwrap();
        assert_eq!(t.id, "t1");
        assert!(t.secret.is_empty());
        assert!(t.active);
    }

    #[test]
    fn target_round_trips_with_camel_case_keys() {
        let mut headers = HashMap::new();
        headers.insert("X-Tenant".to_string(), "acme".to_string());
        let t = Target {
            id: "t2".into(),
            url: "https://example.com".into(),
            event_types: vec!["order.placed".into()],
            headers,
            active: true,
            ..Target::default()
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(r#""eventTypes":["order.placed"]"#));
        assert!(json.contains(r#""createdAt""#));
        let back: Target = serde_json::from_str(&json).unwrap();
        // secret is skipped on both sides, so the round trip preserves
        // everything else.
        assert_eq!(back, t);
    }

    #[test]
    fn callback_event_payload_encodes_as_base64() {
        let ev = CallbackEvent {
            id: "ev1".into(),
            event_type: "order.placed".into(),
            payload: b"{\"a\":1}".to_vec(),
            correlation_id: "c-1".into(),
            ..CallbackEvent::default()
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "order.placed");
        // Go: base64.StdEncoding.EncodeToString([]byte(`{"a":1}`))
        assert_eq!(json["payload"], "eyJhIjoxfQ==");
        assert_eq!(json["correlationId"], "c-1");
        let back: CallbackEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn callback_event_payload_accepts_null_and_omits_empties() {
        let ev: CallbackEvent =
            serde_json::from_str(r#"{"id":"e","type":"t","payload":null}"#).unwrap();
        assert!(ev.payload.is_empty());
        let json = serde_json::to_value(&ev).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("headers"), "omitempty");
        assert!(!obj.contains_key("correlationId"), "omitempty");
    }

    #[test]
    fn authorized_domain_constructors_and_json() {
        let d = AuthorizedDomain::new("example.com");
        assert_eq!(d.domain, "example.com");
        assert!(d.description.is_empty());
        // From<&str> and From<String> convenience.
        assert_eq!(AuthorizedDomain::from("a.com").domain, "a.com");
        assert_eq!(AuthorizedDomain::from("b.com".to_string()).domain, "b.com");

        // Empty description is omitted on the wire.
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["domain"], "example.com");
        assert!(!json.as_object().unwrap().contains_key("description"));

        // A described entry round-trips with a camelCase-free shape.
        let described = AuthorizedDomain {
            domain: "x.com".into(),
            description: "partner".into(),
        };
        let back: AuthorizedDomain =
            serde_json::from_str(&serde_json::to_string(&described).unwrap()).unwrap();
        assert_eq!(back, described);
    }

    #[test]
    fn attempt_json_matches_go_shape() {
        let a = Attempt {
            id: "a1".into(),
            event_id: "ev1".into(),
            target_id: "t1".into(),
            status: 200,
            attempt: 1,
            ..Attempt::default()
        };
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["eventId"], "ev1");
        assert_eq!(json["targetId"], "t1");
        assert_eq!(json["status"], 200);
        assert_eq!(json["attempt"], 1);
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("body"), "omitempty");
        assert!(!obj.contains_key("error"), "omitempty");
        assert!(obj.contains_key("startedAt"));
        assert!(obj.contains_key("finishedAt"));
    }
}
