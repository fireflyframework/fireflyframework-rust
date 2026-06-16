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
use firefly::data::{Page, Pageable};
use lumen_ledger_interfaces::{CreateWalletRequest, WalletFilter, WalletResponse, WalletStatus};
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

    /// Lists every wallet of one owner. (There is intentionally **no**
    /// unfiltered `list_all`: an unauthenticated listing of every wallet would be
    /// a broken-access-control / IDOR enumeration; a real service would expose an
    /// admin-only listing guarded by an authority instead — see the controller.)
    async fn list_by_owner(&self, owner: &str) -> Result<Vec<WalletResponse>, ServiceError>;

    /// Filters wallets by any combination of [`WalletFilter`] criteria
    /// (AND-combined) — translated into a framework
    /// [`Specification`](firefly::data::Specification) the repository runs.
    async fn search(&self, filter: WalletFilter) -> Result<Vec<WalletResponse>, ServiceError>;

    /// A page of wallets in a given status (Spring Data `Page<T>`), honouring
    /// the request's [`Pageable`] (page, size, and sort).
    async fn list_by_status(
        &self,
        status: WalletStatus,
        pageable: Pageable,
    ) -> Result<Page<WalletResponse>, ServiceError>;

    /// Credits an active wallet (atomically, within a transaction).
    async fn deposit(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError>;

    /// Atomically moves `amount` from the `from` wallet to the `to` wallet —
    /// both must be active and `from` must have sufficient funds. The debit and
    /// credit commit together or not at all (`@Transactional`); the updated
    /// source wallet is returned.
    async fn transfer(
        &self,
        from: Uuid,
        to: Uuid,
        amount: i64,
    ) -> Result<WalletResponse, ServiceError>;

    /// Debits an active wallet (rejects an overdraft; atomic).
    async fn withdraw(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError>;

    /// Transitions a wallet's lifecycle status (`active` → `frozen` / `closed`).
    async fn set_status(
        &self,
        id: Uuid,
        status: WalletStatus,
    ) -> Result<WalletResponse, ServiceError>;

    /// Deletes a wallet (idempotent — deleting a missing wallet is not an error).
    async fn delete(&self, id: Uuid) -> Result<(), ServiceError>;
}
