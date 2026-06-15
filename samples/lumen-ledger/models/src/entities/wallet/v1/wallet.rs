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
use uuid::Uuid;

/// The persisted shape of a wallet — one row of the `wallets` table.
/// `status` is stored as its lowercase token (`active`/…); the core
/// layer's mapper translates it to the typed
/// [`WalletStatus`](lumen_ledger_interfaces::WalletStatus) DTO enum.
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
    /// Lifecycle status token (`active` / `frozen` / `closed`).
    pub status: String,
    /// Optimistic-locking version, bumped on every write.
    pub version: i64,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last-update timestamp.
    pub updated_at: DateTime<Utc>,
}
