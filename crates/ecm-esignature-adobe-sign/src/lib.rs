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

//! firefly-ecm-esignature-adobe-sign — the Adobe Sign / Adobe Acrobat Sign
//! [`ESignatureProvider`] adapter (Bearer-token + Adobe Acrobat Sign REST API
//! v6).
//!
//! [`RestProvider`] is a real REST integration over `reqwest`, porting pyfly's
//! `AdobeSignESignatureAdapter`. Every port operation calls the live Adobe
//! Acrobat Sign REST API v6:
//!
//! | Operation | Adobe Acrobat Sign REST v6 call |
//! |---|---|
//! | [`create`](RestProvider::create) | `POST /agreements` (state `IN_PROCESS`) |
//! | [`status`](RestProvider::status) / [`get`](RestProvider::get) | `GET /agreements/{agreementId}` |
//! | [`cancel`](RestProvider::cancel) | `PUT /agreements/{agreementId}/state` (state `CANCELLED`) |
//! | [`recipients`](RestProvider::recipients) | `GET /agreements/{agreementId}/members` |
//! | [`download`](RestProvider::download) | `GET /agreements/{agreementId}/combinedDocument` |
//!
//! It maps Adobe's agreement `status` strings onto [`SignatureStatus`] (see
//! [`map_status`]) and surfaces participant progress and the combined signed
//! PDF.
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_ecm::{ESignatureProvider, SignatureRequest};
//! use firefly_ecm_esignature_adobe_sign::RestProvider;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_ecm::EcmError> {
//! let provider = RestProvider::new(
//!     "https://api.eu1.adobesign.com/api/rest/v6",
//!     "integration-key-or-token",
//! );
//! assert_eq!(provider.name(), "adobe-sign");
//!
//! let id = provider
//!     .create(SignatureRequest {
//!         document_id: "transient-doc-1".into(),
//!         signers: vec!["alice@example.com".into()],
//!         title: "Loan agreement".into(),
//!         provider: "adobesign".into(),
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
pub const VERSION: &str = "26.6.12";

/// Maps an Adobe Sign agreement / participant `status` string onto the
/// framework's [`SignatureStatus`], porting pyfly's `_map_status` table.
/// `OUT_FOR_SIGNATURE`/`WAITING_FOR_MY_SIGNATURE`/`DRAFT`/`WAITING_FOR_OTHERS`
/// are still in flight ([`SignatureStatus::Pending`]), `SIGNED`/`COMPLETED` are
/// [`SignatureStatus::Signed`], `CANCELLED`/`DECLINED` are
/// [`SignatureStatus::Declined`], and `EXPIRED` is [`SignatureStatus::Expired`].
/// Unknown values fall back to [`SignatureStatus::Pending`] (pyfly's `SENT`).
pub fn map_status(value: &str) -> SignatureStatus {
    match value.to_ascii_uppercase().as_str() {
        "OUT_FOR_SIGNATURE" | "WAITING_FOR_MY_SIGNATURE" | "WAITING_FOR_OTHERS" | "DRAFT" => {
            SignatureStatus::Pending
        }
        "SIGNED" | "COMPLETED" => SignatureStatus::Signed,
        "CANCELLED" | "DECLINED" => SignatureStatus::Declined,
        "EXPIRED" => SignatureStatus::Expired,
        _ => SignatureStatus::Pending,
    }
}

/// Parses an Adobe Sign ISO-8601 timestamp into a UTC [`DateTime`], returning
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

/// RestProvider is the real Adobe Sign [`ESignatureProvider`] adapter over
/// `reqwest` (Bearer-token + Adobe Acrobat Sign REST API v6) — the Rust port of
/// pyfly's `AdobeSignESignatureAdapter`.
#[derive(Debug, Clone)]
pub struct RestProvider {
    api_base: String,
    access_token: String,
    http: reqwest::Client,
}

impl RestProvider {
    /// Returns an Adobe Sign REST adapter.
    ///
    /// * `api_base` — e.g. `https://api.eu1.adobesign.com/api/rest/v6`
    ///   (a trailing slash is stripped).
    /// * `access_token` — an integration key or OAuth access token.
    pub fn new(api_base: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            api_base: api_base.into().trim_end_matches('/').to_string(),
            access_token: access_token.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Reuses a caller-provided `reqwest::Client` instead of the default.
    pub fn with_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    fn agreements_url(&self) -> String {
        format!("{}/agreements", self.api_base)
    }

    fn agreement_url(&self, agreement_id: &str) -> String {
        format!("{}/agreements/{}", self.api_base, agreement_id)
    }

    fn members_url(&self, agreement_id: &str) -> String {
        format!("{}/members", self.agreement_url(agreement_id))
    }

    fn combined_document_url(&self, agreement_id: &str) -> String {
        format!("{}/combinedDocument", self.agreement_url(agreement_id))
    }

    /// Lists the participants of agreement `id`, calling
    /// `GET /agreements/{agreementId}/members` (Adobe Acrobat Sign REST API v6,
    /// *getAllParticipantSets*) and projecting each participant's
    /// `memberInfos[]` onto a [`SignerState`] (email plus the participant
    /// `status` mapped through [`map_status`]). The signing-party participant
    /// sets are under `participantSets[]`.
    ///
    /// Returns [`EcmError::NotFound`] when Adobe answers `404`.
    pub async fn recipients(&self, id: &str) -> Result<Vec<SignerState>, EcmError> {
        let resp = self
            .http
            .get(self.members_url(id))
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
        Ok(parse_members(&body))
    }

    /// Downloads the combined signed PDF for agreement `id`, calling
    /// `GET /agreements/{agreementId}/combinedDocument` (Adobe Acrobat Sign REST
    /// API v6, *getCombinedDocument*). Adobe returns the raw PDF bytes
    /// (`Content-Type: application/pdf`), which are returned verbatim.
    ///
    /// Returns [`EcmError::NotFound`] when Adobe answers `404`.
    pub async fn download(&self, id: &str) -> Result<Vec<u8>, EcmError> {
        let resp = self
            .http
            .get(self.combined_document_url(id))
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

/// Projects an Adobe Acrobat Sign `members` response (`participantSets[]` →
/// `memberInfos[]`) onto [`SignerState`]s. Each participant set carries a
/// `status`; each member info carries an `email`.
fn parse_members(body: &serde_json::Value) -> Vec<SignerState> {
    let Some(sets) = body.get("participantSets").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for set in sets {
        let status = set
            .get("status")
            .and_then(|v| v.as_str())
            .map(map_status)
            .unwrap_or(SignatureStatus::Pending);
        let Some(members) = set.get("memberInfos").and_then(|v| v.as_array()) else {
            continue;
        };
        for m in members {
            if let Some(email) = m.get("email").and_then(|v| v.as_str()) {
                out.push(SignerState::pending(email).with_status(status));
            }
        }
    }
    out
}

#[async_trait]
impl ESignatureProvider for RestProvider {
    async fn create(&self, req: SignatureRequest) -> Result<String, EcmError> {
        let participants: Vec<_> = req
            .signers
            .iter()
            .enumerate()
            .map(|(i, email)| {
                json!({
                    "memberInfos": [{ "email": email }],
                    "order": i + 1,
                    "role": "SIGNER",
                })
            })
            .collect();
        let payload = json!({
            "fileInfos": [{ "transientDocumentId": req.document_id }],
            "name": req.title,
            "participantSetsInfo": participants,
            "signatureType": "ESIGN",
            "state": "IN_PROCESS",
            "message": "",
        });

        let resp = self
            .http
            .post(self.agreements_url())
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(provider_err)?;
        let resp = error_for_status(resp)?;
        let body: serde_json::Value = resp.json().await.map_err(provider_err)?;
        body.get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| EcmError::provider("adobe-sign: response missing id"))
    }

    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError> {
        let resp = self
            .http
            .get(self.agreement_url(id))
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
            .unwrap_or("IN_PROCESS");
        Ok(map_status(status))
    }

    /// Returns the full agreement metadata for `id`, calling
    /// `GET /agreements/{agreementId}` (Adobe Acrobat Sign REST API v6,
    /// *getAgreementInfo*) and projecting the response onto an
    /// [`ESignatureEnvelope`]: the mapped agreement-level [`SignatureStatus`],
    /// the provider-side `id`, and the `displayDate`/`createdDate` lifecycle
    /// timestamp when present. `Ok(None)` on a `404`. The Rust port of pyfly's
    /// `AdobeSignESignatureAdapter.get`.
    async fn get(&self, id: &str) -> Result<Option<ESignatureEnvelope>, EcmError> {
        let resp = self
            .http
            .get(self.agreement_url(id))
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
                .unwrap_or("IN_PROCESS"),
        );

        let mut envelope = ESignatureEnvelope::new(id, status)
            .with_provider(self.name())
            .with_provider_envelope_id(id);
        // Adobe surfaces the send time as `displayDate` (falling back to
        // `createdDate`); completion is exposed by `status == SIGNED/COMPLETED`.
        let sent = body
            .get("displayDate")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("createdDate").and_then(|v| v.as_str()));
        if let Some(sent_at) = parse_dt(sent) {
            envelope = envelope.with_sent_at(sent_at);
        }
        Ok(Some(envelope))
    }

    async fn cancel(&self, id: &str) -> Result<(), EcmError> {
        let resp = self
            .http
            .put(format!("{}/state", self.agreement_url(id)))
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .json(&json!({
                "state": "CANCELLED",
                "agreementCancellationInfo": { "comment": "cancelled by app" },
            }))
            .send()
            .await
            .map_err(provider_err)?;
        error_for_status(resp).map(|_| ())
    }

    fn name(&self) -> &str {
        "adobe-sign"
    }
}

/// Wraps a `reqwest` transport error as an [`EcmError::Provider`].
fn provider_err(err: reqwest::Error) -> EcmError {
    EcmError::provider(format!("adobe-sign: {err}"))
}

/// The analog of pyfly's `resp.raise_for_status()`: turns a >= 400 response
/// into an [`EcmError::Provider`] carrying the status code.
fn error_for_status(resp: reqwest::Response) -> Result<reqwest::Response, EcmError> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        Err(EcmError::provider(format!(
            "adobe-sign: HTTP {}",
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
        let boxed: Box<dyn ESignatureProvider> = Box::new(RestProvider::new("http://x", "t"));
        assert_eq!(boxed.name(), "adobe-sign");

        let arc: Arc<dyn ESignatureProvider> = Arc::new(RestProvider::new("http://x", "t"));
        assert_eq!(arc.name(), "adobe-sign");
    }

    #[test]
    fn base_url_trailing_slash_is_stripped() {
        let p = RestProvider::new("https://api.eu1.adobesign.com/api/rest/v6/", "tok");
        assert_eq!(
            p.agreements_url(),
            "https://api.eu1.adobesign.com/api/rest/v6/agreements"
        );
    }

    #[test]
    fn status_mapping_table_matches_pyfly() {
        assert_eq!(map_status("OUT_FOR_SIGNATURE"), SignatureStatus::Pending);
        assert_eq!(
            map_status("WAITING_FOR_MY_SIGNATURE"),
            SignatureStatus::Pending
        );
        assert_eq!(map_status("DRAFT"), SignatureStatus::Pending);
        assert_eq!(map_status("SIGNED"), SignatureStatus::Signed);
        assert_eq!(map_status("COMPLETED"), SignatureStatus::Signed);
        assert_eq!(map_status("CANCELLED"), SignatureStatus::Declined);
        assert_eq!(map_status("DECLINED"), SignatureStatus::Declined);
        assert_eq!(map_status("EXPIRED"), SignatureStatus::Expired);
        assert_eq!(map_status("out_for_signature"), SignatureStatus::Pending);
        assert_eq!(map_status("mystery"), SignatureStatus::Pending);
    }

    #[test]
    fn parse_members_projects_participant_sets() {
        let body = json!({
            "participantSets": [
                {
                    "status": "SIGNED",
                    "memberInfos": [{ "email": "a@x.com" }],
                },
                {
                    "status": "WAITING_FOR_MY_SIGNATURE",
                    "memberInfos": [{ "email": "b@x.com" }, { "email": "c@x.com" }],
                },
            ]
        });
        let s = parse_members(&body);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].email, "a@x.com");
        assert_eq!(s[0].status, SignatureStatus::Signed);
        assert_eq!(s[1].status, SignatureStatus::Pending);
        assert_eq!(s[2].email, "c@x.com");

        assert!(parse_members(&json!({})).is_empty());
    }

    #[test]
    fn provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RestProvider>();
        assert_send_sync::<Arc<dyn ESignatureProvider>>();
    }
}
