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

//! The webhook subsystem's data and port vocabulary — the Rust spelling
//! of the Go `webhooks/interfaces` package: the [`Inbound`] DTO plus the
//! [`Validator`] and [`Processor`] ports.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::error::WebhookError;

/// A single webhook event captured at the ingestion endpoint.
///
/// The JSON shape is identical to the Go struct's tags: `id`,
/// `provider`, `eventType`, `headers`, `payload` (base64, exactly as Go
/// marshals `[]byte`), and `receivedAt` (RFC 3339). Headers are kept in
/// a [`BTreeMap`] so the serialized key order matches Go's sorted map
/// marshaling byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbound {
    /// Unique ingestion id (24 hex chars from 12 random bytes).
    pub id: String,
    /// The provider segment of `POST /api/webhooks/{provider}`.
    pub provider: String,
    /// The `X-Event-Type` header value, when present.
    #[serde(rename = "eventType")]
    pub event_type: String,
    /// First value of every request header, with Go's canonical MIME
    /// header casing (`X-Event-Type`, `Content-Type`, …).
    pub headers: BTreeMap<String, String>,
    /// The raw request body — the exact bytes the signature covers.
    #[serde(with = "base64_bytes")]
    pub payload: Vec<u8>,
    /// UTC ingestion instant.
    #[serde(rename = "receivedAt")]
    pub received_at: DateTime<Utc>,
}

/// Serde adapter encoding `Vec<u8>` as a standard-base64 JSON string —
/// the wire shape Go's `encoding/json` gives `[]byte`.
mod base64_bytes {
    use base64::engine::general_purpose::STANDARD as BASE64_STD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&BASE64_STD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(de)?;
        BASE64_STD.decode(s).map_err(serde::de::Error::custom)
    }
}

/// Vets an HTTP request before the framework persists or dispatches it.
///
/// Common implementations:
/// [`StripeValidator`](crate::StripeValidator),
/// [`GitHubValidator`](crate::GitHubValidator),
/// [`TwilioValidator`](crate::TwilioValidator), and the generic
/// [`HmacValidator`](crate::HmacValidator). Go's `Verify` receives the
/// whole `*http.Request`; the Rust port passes the two parts every
/// canonical scheme consumes — the header map and the raw body bytes.
pub trait Validator: Send + Sync {
    /// The provider key this validator serves (the `{provider}` path
    /// segment, e.g. `"stripe"`).
    fn provider(&self) -> &str;

    /// Verifies the request signature over `body`.
    ///
    /// # Errors
    ///
    /// [`WebhookError::SignatureMismatch`] (or
    /// [`WebhookError::StaleSignature`]) when verification fails.
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), WebhookError>;
}

/// The per-provider downstream handler. The framework invokes
/// [`Processor::process`] on every successfully validated [`Inbound`].
#[async_trait]
pub trait Processor: Send + Sync {
    /// The provider key this processor consumes events for.
    fn provider(&self) -> &str;

    /// Handles one validated event.
    ///
    /// # Errors
    ///
    /// Any error aborts downstream processors and sends the event to
    /// the pipeline's DLQ; use [`WebhookError::processor`] for ad-hoc
    /// failures.
    async fn process(&self, ev: &Inbound) -> Result<(), WebhookError>;
}

/// `Arc<V>` validates by delegating to `V`, so shared validators can be
/// registered without a wrapper type.
impl<V: Validator + ?Sized> Validator for Arc<V> {
    fn provider(&self) -> &str {
        (**self).provider()
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), WebhookError> {
        (**self).verify(headers, body)
    }
}

/// `Arc<P>` processes by delegating to `P`, so callers can keep a
/// handle to a processor (e.g. for assertions or metrics) after
/// registering it.
#[async_trait]
impl<P: Processor + ?Sized> Processor for Arc<P> {
    fn provider(&self) -> &str {
        (**self).provider()
    }

    async fn process(&self, ev: &Inbound) -> Result<(), WebhookError> {
        (**self).process(ev).await
    }
}
