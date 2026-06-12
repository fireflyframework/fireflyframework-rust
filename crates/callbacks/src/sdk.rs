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
