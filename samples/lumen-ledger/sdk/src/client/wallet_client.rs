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

//! The typed wallet API client.

use firefly_client::{ClientError, RestBuilder, RestClient, NO_BODY};
use http::Method;
use lumen_ledger_interfaces::{AmountRequest, CreateWalletRequest, WalletResponse};

/// A typed client for the wallet API, over [`RestClient`]. Each method maps to
/// one endpoint and (de)serialises the shared `-interfaces` DTOs, so a caller
/// programs against the same contract the server enforces.
pub struct WalletClient {
    inner: RestClient,
}

impl WalletClient {
    /// Builds a client against `base_url` (e.g. `http://localhost:8080`).
    #[must_use]
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            inner: RestBuilder::new(base_url).build(),
        }
    }

    /// Wraps an already-configured [`RestClient`] (custom headers, retries,
    /// timeouts, bearer token, …).
    #[must_use]
    pub fn with_client(inner: RestClient) -> Self {
        Self { inner }
    }

    /// `POST /api/v1/wallets` — open a wallet.
    pub async fn create_wallet(
        &self,
        request: &CreateWalletRequest,
    ) -> Result<WalletResponse, ClientError> {
        self.inner
            .request::<_, WalletResponse>(Method::POST, "/api/v1/wallets", Some(request))
            .await
    }

    /// `GET /api/v1/wallets/{id}` — fetch one wallet.
    pub async fn get_wallet(
        &self,
        id: impl std::fmt::Display,
    ) -> Result<WalletResponse, ClientError> {
        let path = format!("/api/v1/wallets/{id}");
        self.inner
            .request::<(), WalletResponse>(Method::GET, &path, NO_BODY)
            .await
    }

    /// `GET /api/v1/wallets?owner=…` — list one owner's wallets.
    pub async fn list_wallets(
        &self,
        owner: impl std::fmt::Display,
    ) -> Result<Vec<WalletResponse>, ClientError> {
        let path = format!("/api/v1/wallets?owner={owner}");
        self.inner
            .request::<(), Vec<WalletResponse>>(Method::GET, &path, NO_BODY)
            .await
    }

    /// `POST /api/v1/wallets/{id}/deposit` — credit a wallet.
    pub async fn deposit(
        &self,
        id: impl std::fmt::Display,
        amount: &AmountRequest,
    ) -> Result<WalletResponse, ClientError> {
        let path = format!("/api/v1/wallets/{id}/deposit");
        self.inner
            .request::<_, WalletResponse>(Method::POST, &path, Some(amount))
            .await
    }

    /// `POST /api/v1/wallets/{id}/withdraw` — debit a wallet.
    pub async fn withdraw(
        &self,
        id: impl std::fmt::Display,
        amount: &AmountRequest,
    ) -> Result<WalletResponse, ClientError> {
        let path = format!("/api/v1/wallets/{id}/withdraw");
        self.inner
            .request::<_, WalletResponse>(Method::POST, &path, Some(amount))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-level contract check: the typed methods line up with the shared
    // DTOs and the `RestClient` request surface. (Network calls are exercised by
    // the `-web` integration test.)
    #[test]
    fn client_constructs() {
        let _client = WalletClient::new("http://localhost:8080");
    }

    // A compile-time contract check: every method's typed result lines up with
    // the shared `-interfaces` DTOs. Never called (the network round-trip is
    // exercised by the `-web` integration test); it exists so a contract drift
    // fails to compile.
    #[allow(dead_code)]
    async fn assert_signatures(client: &WalletClient) -> Result<(), firefly_client::ClientError> {
        let request = CreateWalletRequest::default();
        let amount = AmountRequest::default();
        let _created: WalletResponse = client.create_wallet(&request).await?;
        let _fetched: WalletResponse = client.get_wallet("id").await?;
        let _listed: Vec<WalletResponse> = client.list_wallets("ada").await?;
        let _deposited: WalletResponse = client.deposit("id", &amount).await?;
        let _withdrawn: WalletResponse = client.withdraw("id", &amount).await?;
        Ok(())
    }
}
