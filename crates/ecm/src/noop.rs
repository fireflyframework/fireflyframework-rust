//! In-memory no-op [`ESignatureProvider`] — the public test/dev signer.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::ports::{ESignatureProvider, EcmError, SignatureRequest, SignatureStatus};

/// NoOpESignature marks every signature flow as [`SignatureStatus::Signed`]
/// immediately — the Rust analog of pyfly's `NoOpESignatureAdapter`. It is the
/// public, shippable signer for development and tests (replacing the former
/// test-internal `StaticSigner`), keeping an in-memory envelope map behind a
/// [`tokio::sync::RwLock`].
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
/// signer.cancel(&id).await?;
/// assert_eq!(signer.status(&id).await?, SignatureStatus::Declined);
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct NoOpESignature {
    envelopes: RwLock<HashMap<String, SignatureStatus>>,
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
    /// [`SignatureStatus::Signed`], and returns its generated id.
    async fn create(&self, _req: SignatureRequest) -> Result<String, EcmError> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.envelopes
            .write()
            .await
            .insert(id.clone(), SignatureStatus::Signed);
        Ok(id)
    }

    /// Returns the recorded status of envelope `id`, or [`EcmError::NotFound`].
    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError> {
        self.envelopes
            .read()
            .await
            .get(id)
            .copied()
            .ok_or(EcmError::NotFound)
    }

    /// Marks envelope `id` as [`SignatureStatus::Declined`]; the analog of
    /// pyfly's cancel (which flips the status to `DECLINED`).
    /// [`EcmError::NotFound`] when the envelope is unknown.
    async fn cancel(&self, id: &str) -> Result<(), EcmError> {
        let mut guard = self.envelopes.write().await;
        match guard.get_mut(id) {
            Some(status) => {
                *status = SignatureStatus::Declined;
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
