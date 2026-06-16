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

//! firefly-ecm-esignature-logalty — the Logalty [`ESignatureProvider`]
//! adapter (EU qualified / eIDAS e-signature, REST + `X-Api-Key`).
//!
//! [`RestProvider`] is a real REST integration over `reqwest`, porting pyfly's
//! `LogaltyESignatureAdapter`. Every port operation calls the live Logalty
//! REST API:
//!
//! | Operation | Logalty REST call |
//! |---|---|
//! | [`create`](RestProvider::create) | `POST /envelopes` |
//! | [`status`](RestProvider::status) / [`get`](RestProvider::get) | `GET /envelopes/{envelopeId}` |
//! | [`cancel`](RestProvider::cancel) | `DELETE /envelopes/{envelopeId}` |
//! | [`recipients`](RestProvider::recipients) | `GET /envelopes/{envelopeId}` (projects `signers[]`) |
//! | [`download`](RestProvider::download) | `GET /envelopes/{envelopeId}/document` |
//!
//! It maps Logalty's `status` strings onto [`SignatureStatus`] (see
//! [`map_status`]) and surfaces signer progress, lifecycle timestamps, and the
//! signed PDF.
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_ecm::{ESignatureProvider, SignatureRequest};
//! use firefly_ecm_esignature_logalty::RestProvider;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_ecm::EcmError> {
//! let provider = RestProvider::new(
//!     "https://tenant.logalty.example/api/v1",
//!     "secret-api-key",
//! );
//! assert_eq!(provider.name(), "logalty");
//!
//! let id = provider
//!     .create(SignatureRequest {
//!         document_id: "doc-42".into(),
//!         signers: vec!["alice@example.com".into()],
//!         title: "Sign this".into(),
//!         provider: "logalty".into(),
//!     })
//!     .await?;
//! let _status = provider.status(&id).await?;
//! let _envelope = provider.get(&id).await?;
//! let _recipients = provider.recipients(&id).await?;
//! let _signed_pdf = provider.download(&id).await?;
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firefly_ecm::{
    ESignatureEnvelope, ESignatureProvider, EcmError, SignatureRequest, SignatureStatus,
    SignerState,
};
use serde_json::json;

/// Framework version stamp.
pub const VERSION: &str = "26.6.11";

/// Maps a Logalty `status` string onto the framework's [`SignatureStatus`],
/// porting pyfly's `_map_status` table. `DRAFT`/`SENT`/`PENDING` are still in
/// flight ([`SignatureStatus::Pending`]), `SIGNED`/`COMPLETED` are
/// [`SignatureStatus::Signed`], `DECLINED` is [`SignatureStatus::Declined`],
/// and `EXPIRED` is [`SignatureStatus::Expired`]. Unknown values fall back to
/// [`SignatureStatus::Pending`] (pyfly's `SENT`).
pub fn map_status(value: &str) -> SignatureStatus {
    match value.to_ascii_uppercase().as_str() {
        "DRAFT" | "SENT" | "PENDING" => SignatureStatus::Pending,
        "SIGNED" | "COMPLETED" => SignatureStatus::Signed,
        "DECLINED" => SignatureStatus::Declined,
        "EXPIRED" => SignatureStatus::Expired,
        _ => SignatureStatus::Pending,
    }
}

/// Parses a Logalty ISO-8601 timestamp into a UTC [`DateTime`], returning
/// `None` for an empty/absent/unparseable value.
fn parse_dt(value: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// RestProvider is the real Logalty [`ESignatureProvider`] adapter over
/// `reqwest` (eIDAS REST + `X-Api-Key`) — the Rust port of pyfly's
/// `LogaltyESignatureAdapter`.
#[derive(Debug, Clone)]
pub struct RestProvider {
    api_base: String,
    api_key: String,
    http: reqwest::Client,
}

impl RestProvider {
    /// Returns a Logalty REST adapter.
    ///
    /// * `api_base` — the tenant-specific API root (a trailing slash is
    ///   stripped).
    /// * `api_key` — the Logalty API key, sent as the `X-Api-Key` header.
    pub fn new(api_base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            api_base: api_base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Reuses a caller-provided `reqwest::Client` instead of the default.
    pub fn with_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    fn envelopes_url(&self) -> String {
        format!("{}/envelopes", self.api_base)
    }

    fn envelope_url(&self, envelope_id: &str) -> String {
        format!("{}/envelopes/{}", self.api_base, envelope_id)
    }

    fn document_url(&self, envelope_id: &str) -> String {
        format!("{}/document", self.envelope_url(envelope_id))
    }

    /// Fetches envelope `id` and returns the parsed JSON body, mapping a `404`
    /// to [`EcmError::NotFound`]. Shared by [`status`](Self::status),
    /// [`get`](RestProvider::get), and [`recipients`](Self::recipients).
    async fn fetch_envelope(&self, id: &str) -> Result<serde_json::Value, EcmError> {
        let resp = self
            .http
            .get(self.envelope_url(id))
            .header("X-Api-Key", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(provider_err)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        let resp = error_for_status(resp)?;
        resp.json().await.map_err(provider_err)
    }

    /// Lists the signers of envelope `id`, calling
    /// `GET /envelopes/{envelopeId}` (Logalty REST) and projecting the
    /// response's `signers[]` onto [`SignerState`]s (email plus the per-signer
    /// `status` mapped through [`map_status`], and `signedAt` when present).
    /// Logalty embeds signer detail in the envelope resource rather than a
    /// dedicated recipients endpoint.
    ///
    /// Returns [`EcmError::NotFound`] when Logalty answers `404`.
    pub async fn recipients(&self, id: &str) -> Result<Vec<SignerState>, EcmError> {
        let body = self.fetch_envelope(id).await?;
        Ok(parse_signers(&body))
    }

    /// Downloads the signed PDF for envelope `id`, calling
    /// `GET /envelopes/{envelopeId}/document` (Logalty REST). Logalty returns
    /// the raw signed-document bytes (`Content-Type: application/pdf`), which
    /// are returned verbatim.
    ///
    /// Returns [`EcmError::NotFound`] when Logalty answers `404`.
    pub async fn download(&self, id: &str) -> Result<Vec<u8>, EcmError> {
        let resp = self
            .http
            .get(self.document_url(id))
            .header("X-Api-Key", &self.api_key)
            .header("Accept", "application/pdf")
            .send()
            .await
            .map_err(provider_err)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        let resp = error_for_status(resp)?;
        let bytes = resp.bytes().await.map_err(provider_err)?;
        Ok(bytes.to_vec())
    }
}

/// Projects a Logalty envelope JSON body's `signers[]` onto [`SignerState`]s.
fn parse_signers(body: &serde_json::Value) -> Vec<SignerState> {
    let Some(signers) = body.get("signers").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    signers
        .iter()
        .filter_map(|s| {
            let email = s.get("email").and_then(|v| v.as_str())?.to_string();
            let status = s
                .get("status")
                .and_then(|v| v.as_str())
                .map(map_status)
                .unwrap_or(SignatureStatus::Pending);
            let signed_at = parse_dt(s.get("signedAt").and_then(|v| v.as_str()));
            let mut state = SignerState::pending(email).with_status(status);
            if let Some(ts) = signed_at {
                state = state.with_signed_at(ts);
            }
            Some(state)
        })
        .collect()
}

#[async_trait]
impl ESignatureProvider for RestProvider {
    async fn create(&self, req: SignatureRequest) -> Result<String, EcmError> {
        let signers: Vec<_> = req
            .signers
            .iter()
            .map(|email| json!({ "name": email, "email": email, "role": "signer" }))
            .collect();
        let payload = json!({
            "documentId": req.document_id,
            "subject": req.title,
            "message": "",
            "signers": signers,
        });

        let resp = self
            .http
            .post(self.envelopes_url())
            .header("X-Api-Key", &self.api_key)
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(provider_err)?;
        let resp = error_for_status(resp)?;
        let body: serde_json::Value = resp.json().await.map_err(provider_err)?;
        body.get("envelopeId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| EcmError::provider("logalty: response missing envelopeId"))
    }

    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError> {
        let body = self.fetch_envelope(id).await?;
        let status = body
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("SENT");
        Ok(map_status(status))
    }

    /// Returns the full envelope metadata for `id`, calling
    /// `GET /envelopes/{envelopeId}` (Logalty REST) and projecting the response
    /// onto an [`ESignatureEnvelope`]: the mapped envelope-level
    /// [`SignatureStatus`], the provider-side envelope id, the `sentAt` /
    /// `signedAt` lifecycle timestamps, and (when present) the per-signer
    /// [`SignerState`] breakdown. `Ok(None)` on a `404`. The Rust port of
    /// pyfly's `LogaltyESignatureAdapter.get`.
    async fn get(&self, id: &str) -> Result<Option<ESignatureEnvelope>, EcmError> {
        let body = match self.fetch_envelope(id).await {
            Ok(body) => body,
            Err(EcmError::NotFound) => return Ok(None),
            Err(err) => return Err(err),
        };
        let status = map_status(
            body.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("SENT"),
        );

        let mut envelope = ESignatureEnvelope::new(id, status)
            .with_provider(self.name())
            .with_provider_envelope_id(id);
        if let Some(sent_at) = parse_dt(body.get("sentAt").and_then(|v| v.as_str())) {
            envelope = envelope.with_sent_at(sent_at);
        }
        if let Some(signed_at) = parse_dt(body.get("signedAt").and_then(|v| v.as_str())) {
            envelope = envelope.with_signed_at(signed_at);
        }
        let signers = parse_signers(&body);
        if !signers.is_empty() {
            envelope = envelope.with_signers(signers);
        }
        Ok(Some(envelope))
    }

    async fn cancel(&self, id: &str) -> Result<(), EcmError> {
        let resp = self
            .http
            .delete(self.envelope_url(id))
            .header("X-Api-Key", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(provider_err)?;
        error_for_status(resp).map(|_| ())
    }

    fn name(&self) -> &str {
        "logalty"
    }
}

/// Wraps a `reqwest` transport error as an [`EcmError::Provider`].
fn provider_err(err: reqwest::Error) -> EcmError {
    EcmError::provider(format!("logalty: {err}"))
}

/// The analog of pyfly's `resp.raise_for_status()`: turns a >= 400 response
/// into an [`EcmError::Provider`] carrying the status code.
fn error_for_status(resp: reqwest::Response) -> Result<reqwest::Response, EcmError> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        Err(EcmError::provider(format!(
            "logalty: HTTP {}",
            status.as_u16()
        )))
    } else {
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn implements_port() {
        let boxed: Box<dyn ESignatureProvider> = Box::new(RestProvider::new("http://x", "k"));
        assert_eq!(boxed.name(), "logalty");

        let arc: Arc<dyn ESignatureProvider> = Arc::new(RestProvider::new("http://x", "k"));
        assert_eq!(arc.name(), "logalty");
    }

    #[test]
    fn base_url_trailing_slash_is_stripped() {
        let p = RestProvider::new("https://tenant.logalty.example/api/v1/", "key");
        assert_eq!(
            p.envelopes_url(),
            "https://tenant.logalty.example/api/v1/envelopes"
        );
    }

    #[test]
    fn status_mapping_table_matches_pyfly() {
        assert_eq!(map_status("DRAFT"), SignatureStatus::Pending);
        assert_eq!(map_status("SENT"), SignatureStatus::Pending);
        assert_eq!(map_status("PENDING"), SignatureStatus::Pending);
        assert_eq!(map_status("SIGNED"), SignatureStatus::Signed);
        assert_eq!(map_status("COMPLETED"), SignatureStatus::Signed);
        assert_eq!(map_status("DECLINED"), SignatureStatus::Declined);
        assert_eq!(map_status("EXPIRED"), SignatureStatus::Expired);
        assert_eq!(map_status("signed"), SignatureStatus::Signed);
        assert_eq!(map_status("mystery"), SignatureStatus::Pending);
    }

    #[test]
    fn parse_signers_reads_envelope_shape() {
        let body = json!({
            "signers": [
                { "email": "a@x.com", "status": "SIGNED", "signedAt": "2026-06-02T12:30:00Z" },
                { "email": "b@x.com", "status": "SENT" },
            ]
        });
        let s = parse_signers(&body);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].email, "a@x.com");
        assert_eq!(s[0].status, SignatureStatus::Signed);
        assert!(s[0].signed_at.is_some());
        assert_eq!(s[1].status, SignatureStatus::Pending);
        assert!(s[1].signed_at.is_none());

        assert!(parse_signers(&json!({})).is_empty());
    }

    #[test]
    fn provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RestProvider>();
        assert_send_sync::<Arc<dyn ESignatureProvider>>();
    }
}
