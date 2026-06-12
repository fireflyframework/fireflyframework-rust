//! Typed client for the callbacks admin REST API — the Rust spelling
//! of the Go `callbacks/sdk` sub-package, built on
//! [`firefly_client::RestClient`] exactly as the Go SDK builds on
//! `client.NewREST`.

use http::Method;

use firefly_client::{ClientError, RestBuilder, RestClient, NO_BODY};

use crate::interfaces::Target;

/// The typed callbacks-admin SDK — Go's `sdk.Client`, renamed so it can
/// be re-exported flat from the crate root.
///
/// Inherits everything [`RestClient`] does automatically: JSON
/// encoding, `X-Correlation-Id` propagation from the kernel task-local
/// scope, and retry with exponential backoff on 429 / 5xx.
#[derive(Debug, Clone)]
pub struct CallbacksClient {
    rc: RestClient,
}

impl CallbacksClient {
    /// Returns a client targeting `base_url` — Go's `sdk.New(baseURL)`.
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            rc: RestBuilder::new(base_url).build(),
        }
    }

    /// Returns every registered callback target
    /// (`GET /callbacks/targets`).
    ///
    /// # Errors
    ///
    /// See [`ClientError`]; non-2xx admin responses surface as
    /// [`ClientError::Problem`].
    pub async fn targets(&self) -> Result<Vec<Target>, ClientError> {
        self.rc
            .request(Method::GET, "/callbacks/targets", NO_BODY)
            .await
    }

    /// Registers or updates a target (`POST /callbacks/targets`).
    /// [`Target::secret`] is never sent — it is skipped by serde just
    /// as Go's `json:"-"` keeps it off the wire.
    ///
    /// # Errors
    ///
    /// See [`ClientError`]; non-2xx admin responses surface as
    /// [`ClientError::Problem`].
    pub async fn upsert(&self, target: &Target) -> Result<Target, ClientError> {
        self.rc
            .request(Method::POST, "/callbacks/targets", Some(target))
            .await
    }

    /// Removes a target by id (`DELETE /callbacks/targets/{id}`).
    ///
    /// # Errors
    ///
    /// See [`ClientError`]; non-2xx admin responses surface as
    /// [`ClientError::Problem`].
    pub async fn delete(&self, id: &str) -> Result<(), ClientError> {
        self.rc
            .request(Method::DELETE, &format!("/callbacks/targets/{id}"), NO_BODY)
            .await
    }
}
