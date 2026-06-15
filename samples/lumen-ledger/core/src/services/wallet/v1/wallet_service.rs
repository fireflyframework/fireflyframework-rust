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

//! The [`WalletService`] `@Service` interface.

use async_trait::async_trait;
use lumen_ledger_interfaces::{CreateWalletRequest, WalletResponse};
use uuid::Uuid;

use super::service_error::ServiceError;

/// The wallet application service — Spring's `@Service` interface.
/// Object-safe (`dyn WalletService`) so the controller autowires it
/// as a port.
#[async_trait]
pub trait WalletService: Send + Sync {
    /// Opens a new wallet from a validated request.
    async fn create(&self, request: CreateWalletRequest) -> Result<WalletResponse, ServiceError>;

    /// Fetches a wallet by id.
    async fn get(&self, id: Uuid) -> Result<WalletResponse, ServiceError>;

    /// Lists every wallet of one owner.
    async fn list_by_owner(&self, owner: &str) -> Result<Vec<WalletResponse>, ServiceError>;

    /// Credits an active wallet.
    async fn deposit(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError>;

    /// Debits an active wallet (rejects an overdraft).
    async fn withdraw(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError>;
}
