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

//! The [`Wallet`] persistence entity.

use chrono::{DateTime, Utc};
use lumen_ledger_interfaces::WalletStatus;
use uuid::Uuid;

/// The persisted shape of a wallet — one row of the `wallets` table.
///
/// `status` is the typed [`WalletStatus`] enum end-to-end; the token↔enum
/// conversion happens exactly once, at the row boundary (the repository's
/// `RowMapper`/`RowWriter`) — the `@Enumerated(STRING)` analog. `created_at` /
/// `updated_at` and `version` are managed by the store (the framework `Auditor`
/// and the `@Version` optimistic-locking column), not by the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wallet {
    /// Primary key.
    pub id: Uuid,
    /// Human-facing account number (e.g. `"WAL-00001"`).
    pub account_number: String,
    /// Owner display name.
    pub owner: String,
    /// Balance in minor units (cents).
    pub balance: i64,
    /// ISO-4217 currency code.
    pub currency: String,
    /// Lifecycle status (`@Enumerated(STRING)`: stored as its lowercase token).
    pub status: WalletStatus,
    /// Optimistic-locking version (`@Version`) — bumped by the store on update.
    pub version: i64,
    /// Creation timestamp (`@CreatedDate`, stamped by the store on insert).
    pub created_at: DateTime<Utc>,
    /// Last-update timestamp (`@LastModifiedDate`, stamped on every write).
    pub updated_at: DateTime<Utc>,
}
