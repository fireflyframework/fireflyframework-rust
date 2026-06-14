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

//! firefly-ecm-esignature-docusign — the DocuSign [`ESignatureProvider`]
//! adapter (Bearer-token + DocuSign eSignature REST API v2.1).
//!
//! [`RestProvider`] is a real REST integration over `reqwest`, porting pyfly's
//! `DocuSignESignatureAdapter`. Every port operation calls the live DocuSign
//! eSignature REST API v2.1:
//!
//! | Operation | DocuSign REST v2.1 call |
//! |---|---|
//! | [`create`](RestProvider::create) | `POST /v2.1/accounts/{accountId}/envelopes` (status `sent`) |
//! | [`status`](RestProvider::status) / [`get`](RestProvider::get) | `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}` |
//! | [`cancel`](RestProvider::cancel) | `PUT /v2.1/accounts/{accountId}/envelopes/{envelopeId}` (status `voided`) |
//! | [`recipients`](RestProvider::recipients) | `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}/recipients` |
//! | [`download`](RestProvider::download) | `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}/documents/combined` |
//!
//! It maps DocuSign's envelope `status` strings onto [`SignatureStatus`] (see
//! [`map_status`]) and surfaces per-signer progress, lifecycle timestamps, and
//! the combined signed PDF.
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_ecm::{ESignatureProvider, SignatureRequest};
//! use firefly_ecm_esignature_docusign::RestProvider;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_ecm::EcmError> {
//! let provider = RestProvider::new(
//!     "https://demo.docusign.net/restapi",
//!     "account-123",
//!     "bearer-token",
//! );
//! assert_eq!(provider.name(), "docusign");
//!
//! let id = provider
//!     .create(SignatureRequest {
//!         document_id: "doc-1".into(),
//!         signers: vec!["alice@example.com".into()],
//!         title: "Sign please".into(),
//!         provider: "docusign".into(),
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
pub const VERSION: &str = "26.6.4";

/// Maps a DocuSign envelope `status` string onto the framework's
/// [`SignatureStatus`], porting pyfly's `_map_status` table. DocuSign's
/// `created`/`sent`/`delivered` are still in flight ([`SignatureStatus::Pending`]),
/// `completed` is [`SignatureStatus::Signed`], `declined`/`voided` are
/// [`SignatureStatus::Declined`], and `expired` is [`SignatureStatus::Expired`].
/// Unknown values fall back to [`SignatureStatus::Pending`] (pyfly's `SENT`).
pub fn map_status(value: &str) -> SignatureStatus {
    match value.to_ascii_lowercase().as_str() {
        "created" | "sent" | "delivered" => SignatureStatus::Pending,
        "completed" => SignatureStatus::Signed,
        "declined" | "voided" => SignatureStatus::Declined,
        "expired" => SignatureStatus::Expired,
        _ => SignatureStatus::Pending,
    }
}

/// Parses a DocuSign ISO-8601 timestamp (e.g. `2026-06-01T10:00:00.0000000Z`)
/// into a UTC [`DateTime`], returning `None` for an empty/absent/unparseable
/// value — the analog of pyfly's `_parse`.
fn parse_dt(value: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// RestProvider is the real DocuSign [`ESignatureProvider`] adapter over
/// `reqwest` (Bearer-token + DocuSign eSignature REST API v2.1) — the Rust port
/// of pyfly's `DocuSignESignatureAdapter`.
#[derive(Debug, Clone)]
pub struct RestProvider {
    base_url: String,
    account_id: String,
    access_token: String,
    http: reqwest::Client,
}

impl RestProvider {
    /// Returns a DocuSign REST adapter.
    ///
    /// * `base_url` — e.g. `https://demo.docusign.net/restapi` (a trailing
    ///   slash is stripped).
    /// * `account_id` — the DocuSign account id.
    /// * `access_token` — a long-lived OAuth bearer token (refresh is the
    ///   caller's responsibility).
    pub fn new(
        base_url: impl Into<String>,
        account_id: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            account_id: account_id.into(),
            access_token: access_token.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Reuses a caller-provided `reqwest::Client` (connection pooling,
    /// custom timeouts/TLS) instead of the default.
    pub fn with_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    fn envelopes_url(&self) -> String {
        format!(
            "{}/v2.1/accounts/{}/envelopes",
            self.base_url, self.account_id
        )
    }

    fn envelope_url(&self, envelope_id: &str) -> String {
        format!(
            "{}/v2.1/accounts/{}/envelopes/{}",
            self.base_url, self.account_id, envelope_id
        )
    }

    fn recipients_url(&self, envelope_id: &str) -> String {
        format!("{}/recipients", self.envelope_url(envelope_id))
    }

    fn combined_documents_url(&self, envelope_id: &str) -> String {
        format!("{}/documents/combined", self.envelope_url(envelope_id))
    }

    /// Lists the recipients of envelope `id`, calling
    /// `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}/recipients`
    /// (DocuSign eSignature REST API v2.1, *EnvelopeRecipients: list*) and
    /// projecting each `signers[]` entry onto a [`SignerState`] (email plus the
    /// per-recipient `status` mapped through [`map_status`], and the
    /// `signedDateTime` when present).
    ///
    /// Returns [`EcmError::NotFound`] when DocuSign answers `404` (no such
    /// envelope).
    pub async fn recipients(&self, id: &str) -> Result<Vec<SignerState>, EcmError> {
        let resp = self
            .http
            .get(self.recipients_url(id))
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(provider_err)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        let resp = error_for_status(resp)?;
        let body: serde_json::Value = resp.json().await.map_err(provider_err)?;
        Ok(parse_signers(&body))
    }

    /// Downloads the combined signed PDF for envelope `id`, calling
    /// `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}/documents/combined`
    /// (DocuSign eSignature REST API v2.1, *EnvelopeDocuments: get*, the
    /// `combined` pseudo-document id). DocuSign returns the raw PDF bytes
    /// (`Content-Type: application/pdf`), which are returned verbatim.
    ///
    /// Returns [`EcmError::NotFound`] when DocuSign answers `404`.
    pub async fn download(&self, id: &str) -> Result<Vec<u8>, EcmError> {
        let resp = self
            .http
            .get(self.combined_documents_url(id))
            .bearer_auth(&self.access_token)
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

/// Projects a DocuSign envelope/recipients JSON body's `recipients.signers[]`
/// (the *get envelope* shape) **or** top-level `signers[]` (the *list
/// recipients* shape) onto [`SignerState`]s.
fn parse_signers(body: &serde_json::Value) -> Vec<SignerState> {
    let signers = body
        .get("recipients")
        .and_then(|r| r.get("signers"))
        .or_else(|| body.get("signers"))
        .and_then(|v| v.as_array());
    let Some(signers) = signers else {
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
            let signed_at = parse_dt(s.get("signedDateTime").and_then(|v| v.as_str()));
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
            .enumerate()
            .map(|(i, email)| {
                let n = (i + 1).to_string();
                json!({
                    "email": email,
                    "name": email,
                    "recipientId": n,
                    "routingOrder": n,
                })
            })
            .collect();
        let payload = json!({
            "emailSubject": req.title,
            "emailBlurb": "",
            "documents": [{
                "documentId": req.document_id,
                "name": "document.pdf",
                "fileExtension": "pdf",
            }],
            "recipients": { "signers": signers },
            "status": "sent",
        });

        let resp = self
            .http
            .post(self.envelopes_url())
            .bearer_auth(&self.access_token)
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
            .ok_or_else(|| EcmError::provider("docusign: response missing envelopeId"))
    }

    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError> {
        let resp = self
            .http
            .get(self.envelope_url(id))
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(provider_err)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(EcmError::NotFound);
        }
        let resp = error_for_status(resp)?;
        let body: serde_json::Value = resp.json().await.map_err(provider_err)?;
        let status = body
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("sent");
        Ok(map_status(status))
    }

    /// Returns the full envelope metadata for `id`, calling
    /// `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}` (DocuSign
    /// eSignature REST API v2.1, *Envelopes: get*) and projecting the response
    /// onto an [`ESignatureEnvelope`]: the mapped envelope-level
    /// [`SignatureStatus`], the provider-side `envelopeId`, the `sentDateTime` /
    /// `completedDateTime` lifecycle timestamps, and (when DocuSign inlines
    /// `recipients.signers[]`) the per-[`SignerState`] breakdown. `Ok(None)` on
    /// a `404`. The Rust port of pyfly's `DocuSignESignatureAdapter.get`.
    async fn get(&self, id: &str) -> Result<Option<ESignatureEnvelope>, EcmError> {
        let resp = self
            .http
            .get(self.envelope_url(id))
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(provider_err)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = error_for_status(resp)?;
        let body: serde_json::Value = resp.json().await.map_err(provider_err)?;
        let status = map_status(
            body.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("sent"),
        );

        let mut envelope = ESignatureEnvelope::new(id, status)
            .with_provider(self.name())
            .with_provider_envelope_id(id);
        if let Some(sent_at) = parse_dt(body.get("sentDateTime").and_then(|v| v.as_str())) {
            envelope = envelope.with_sent_at(sent_at);
        }
        if let Some(signed_at) = parse_dt(body.get("completedDateTime").and_then(|v| v.as_str())) {
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
            .put(self.envelope_url(id))
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .json(&json!({
                "status": "voided",
                "voidedReason": "cancelled by application",
            }))
            .send()
            .await
            .map_err(provider_err)?;
        error_for_status(resp).map(|_| ())
    }

    fn name(&self) -> &str {
        "docusign"
    }
}

/// Wraps a `reqwest` transport error as an [`EcmError::Provider`].
fn provider_err(err: reqwest::Error) -> EcmError {
    EcmError::provider(format!("docusign: {err}"))
}

/// The analog of pyfly's `resp.raise_for_status()`: turns a >= 400 response
/// into an [`EcmError::Provider`] carrying the status code, while leaving 2xx
/// (and any already-handled 404) responses untouched.
fn error_for_status(resp: reqwest::Response) -> Result<reqwest::Response, EcmError> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        Err(EcmError::provider(format!(
            "docusign: HTTP {}",
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

    // -----------------------------------------------------------------------
    // Go: TestImplementsPort — `var _ ecm.ESignatureProvider = New(Config{})`.
    // The Rust analog: the adapter coerces to the object-safe port behind
    // Arc/Box, which fails to compile if the trait is not implemented.
    // -----------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let boxed: Box<dyn ESignatureProvider> = Box::new(RestProvider::new("http://x", "a", "t"));
        assert_eq!(boxed.name(), "docusign");

        let arc: Arc<dyn ESignatureProvider> = Arc::new(RestProvider::new("http://x", "a", "t"));
        assert_eq!(arc.name(), "docusign");
    }

    #[test]
    fn base_url_trailing_slash_is_stripped() {
        let p = RestProvider::new("https://demo.docusign.net/restapi/", "acct", "tok");
        assert_eq!(
            p.envelopes_url(),
            "https://demo.docusign.net/restapi/v2.1/accounts/acct/envelopes"
        );
    }

    #[test]
    fn status_mapping_table_matches_pyfly() {
        assert_eq!(map_status("created"), SignatureStatus::Pending);
        assert_eq!(map_status("sent"), SignatureStatus::Pending);
        assert_eq!(map_status("delivered"), SignatureStatus::Pending);
        assert_eq!(map_status("completed"), SignatureStatus::Signed);
        assert_eq!(map_status("declined"), SignatureStatus::Declined);
        assert_eq!(map_status("voided"), SignatureStatus::Declined);
        assert_eq!(map_status("expired"), SignatureStatus::Expired);
        assert_eq!(map_status("COMPLETED"), SignatureStatus::Signed);
        assert_eq!(map_status("mystery"), SignatureStatus::Pending);
    }

    #[test]
    fn parse_dt_handles_empty_and_rfc3339() {
        assert!(parse_dt(None).is_none());
        assert!(parse_dt(Some("")).is_none());
        assert!(parse_dt(Some("not-a-date")).is_none());
        let dt = parse_dt(Some("2026-06-01T10:00:00Z")).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-06-01T10:00:00+00:00");
    }

    #[test]
    fn parse_signers_reads_both_shapes() {
        // get-envelope shape: recipients.signers[]
        let env = json!({
            "recipients": { "signers": [
                { "email": "a@x.com", "status": "completed", "signedDateTime": "2026-06-02T12:30:00Z" },
                { "email": "b@x.com", "status": "sent" },
            ]}
        });
        let s = parse_signers(&env);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].email, "a@x.com");
        assert_eq!(s[0].status, SignatureStatus::Signed);
        assert!(s[0].signed_at.is_some());
        assert_eq!(s[1].status, SignatureStatus::Pending);
        assert!(s[1].signed_at.is_none());

        // list-recipients shape: top-level signers[]
        let recips = json!({ "signers": [{ "email": "c@x.com", "status": "declined" }] });
        let s = parse_signers(&recips);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].status, SignatureStatus::Declined);

        // no signers
        assert!(parse_signers(&json!({})).is_empty());
    }

    #[test]
    fn provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RestProvider>();
        assert_send_sync::<Arc<dyn ESignatureProvider>>();
    }
}
