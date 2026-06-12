//! In-memory no-op [`ESignatureProvider`] — the public test/dev signer.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;

use crate::ports::{
    ESignatureEnvelope, ESignatureProvider, EcmError, SignatureRequest, SignatureStatus,
    SignerState,
};

/// NoOpESignature marks every signature flow as [`SignatureStatus::Signed`]
/// immediately — the Rust analog of pyfly's `NoOpESignatureAdapter`. It is the
/// public, shippable signer for development and tests (replacing the former
/// test-internal `StaticSigner`), keeping an in-memory map of full
/// [`ESignatureEnvelope`] metadata behind a [`tokio::sync::RwLock`] — so
/// [`ESignatureProvider::get`] returns the same `sent_at` / `signed_at` /
/// per-signer detail pyfly's no-op adapter exposes.
///
/// # Example
///
/// ```
/// use firefly_ecm::{ESignatureProvider, NoOpESignature, SignatureRequest, SignatureStatus};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), firefly_ecm::EcmError> {
/// let signer = NoOpESignature::new();
/// let id = signer
///     .create(SignatureRequest { document_id: "d-1".into(), ..Default::default() })
///     .await?;
/// assert_eq!(signer.status(&id).await?, SignatureStatus::Signed);
///
/// // `get` returns the full envelope metadata (status + timestamps).
/// let envelope = signer.get(&id).await?.unwrap();
/// assert_eq!(envelope.status, SignatureStatus::Signed);
/// assert!(envelope.signed_at.is_some());
///
/// signer.cancel(&id).await?;
/// assert_eq!(signer.status(&id).await?, SignatureStatus::Declined);
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct NoOpESignature {
    envelopes: RwLock<HashMap<String, ESignatureEnvelope>>,
}

impl NoOpESignature {
    /// Returns a fresh no-op signer with an empty envelope map.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ESignatureProvider for NoOpESignature {
    /// Creates an envelope, records it as immediately
    /// [`SignatureStatus::Signed`] with `sent_at` / `signed_at` stamped to
    /// now and a per-signer breakdown derived from the request, and returns
    /// its generated id — the analog of pyfly's `NoOpESignatureAdapter.send`.
    async fn create(&self, req: SignatureRequest) -> Result<String, EcmError> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = Utc::now();
        let signers = req
            .signers
            .iter()
            .map(|email| {
                SignerState::pending(email)
                    .with_status(SignatureStatus::Signed)
                    .with_signed_at(now)
            })
            .collect();
        let envelope = ESignatureEnvelope::new(&id, SignatureStatus::Signed)
            .with_provider(self.name())
            .with_document_id(req.document_id)
            .with_provider_envelope_id(uuid::Uuid::new_v4().simple().to_string())
            .with_sent_at(now)
            .with_signed_at(now)
            .with_signers(signers);
        self.envelopes.write().await.insert(id.clone(), envelope);
        Ok(id)
    }

    /// Returns the recorded status of envelope `id`, or [`EcmError::NotFound`].
    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError> {
        self.envelopes
            .read()
            .await
            .get(id)
            .map(|env| env.status)
            .ok_or(EcmError::NotFound)
    }

    /// Returns the full [`ESignatureEnvelope`] metadata for `id`, or `None`
    /// when unknown — the analog of pyfly's `NoOpESignatureAdapter.get`.
    async fn get(&self, id: &str) -> Result<Option<ESignatureEnvelope>, EcmError> {
        Ok(self.envelopes.read().await.get(id).cloned())
    }

    /// Marks envelope `id` as [`SignatureStatus::Declined`]; the analog of
    /// pyfly's cancel (which flips the status to `DECLINED`).
    /// [`EcmError::NotFound`] when the envelope is unknown.
    async fn cancel(&self, id: &str) -> Result<(), EcmError> {
        let mut guard = self.envelopes.write().await;
        match guard.get_mut(id) {
            Some(envelope) => {
                envelope.status = SignatureStatus::Declined;
                Ok(())
            }
            None => Err(EcmError::NotFound),
        }
    }

    /// Human-readable provider identifier, matching pyfly's `name = "noop"`.
    fn name(&self) -> &str {
        "noop"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Port of pyfly test_esignature_round_trip: request -> SIGNED, get, cancel.
    #[tokio::test]
    async fn round_trip_signs_immediately() {
        let signer = NoOpESignature::new();
        assert_eq!(signer.name(), "noop");

        let id = signer
            .create(SignatureRequest {
                document_id: "d-1".into(),
                signers: vec!["a@x.com".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!id.is_empty());
        assert_eq!(signer.status(&id).await.unwrap(), SignatureStatus::Signed);

        signer.cancel(&id).await.unwrap();
        assert_eq!(signer.status(&id).await.unwrap(), SignatureStatus::Declined);
    }

    // Port of pyfly test_esignature_round_trip's `service.get(envelope.id)`
    // assertion: `get` returns the full envelope metadata (not just status).
    #[tokio::test]
    async fn get_returns_full_envelope_metadata() {
        let signer = NoOpESignature::new();
        let id = signer
            .create(SignatureRequest {
                document_id: "d-1".into(),
                signers: vec!["a@x.com".into(), "b@x.com".into()],
                title: "NDA".into(),
                provider: "noop".into(),
            })
            .await
            .unwrap();

        let envelope = signer.get(&id).await.unwrap().expect("envelope present");
        assert_eq!(envelope.id, id);
        assert_eq!(envelope.provider, "noop");
        assert_eq!(envelope.document_id, "d-1");
        assert_eq!(envelope.status, SignatureStatus::Signed);
        assert!(envelope.provider_envelope_id.is_some());
        assert!(envelope.sent_at.is_some());
        assert!(envelope.signed_at.is_some());
        // Per-signer breakdown derived from the request's signers.
        assert_eq!(envelope.signers.len(), 2);
        assert!(envelope
            .signers
            .iter()
            .all(|s| s.status == SignatureStatus::Signed && s.signed_at.is_some()));
        let emails: Vec<&str> = envelope.signers.iter().map(|s| s.email.as_str()).collect();
        assert_eq!(emails, vec!["a@x.com", "b@x.com"]);

        // After cancel the status flips on the same envelope.
        signer.cancel(&id).await.unwrap();
        let cancelled = signer.get(&id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, SignatureStatus::Declined);
    }

    #[tokio::test]
    async fn get_of_unknown_envelope_is_none() {
        let signer = NoOpESignature::new();
        assert!(signer.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn status_and_cancel_of_unknown_envelope_are_not_found() {
        let signer = NoOpESignature::new();
        assert!(signer.status("nope").await.unwrap_err().is_not_found());
        assert!(signer.cancel("nope").await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn create_yields_distinct_ids() {
        let signer = NoOpESignature::new();
        let a = signer.create(SignatureRequest::default()).await.unwrap();
        let b = signer.create(SignatureRequest::default()).await.unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let signer: Arc<dyn ESignatureProvider> = Arc::new(NoOpESignature::new());
        let id = signer.create(SignatureRequest::default()).await.unwrap();
        assert_eq!(signer.status(&id).await.unwrap(), SignatureStatus::Signed);
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoOpESignature>();
    }
}
