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

//! The [`WalletResponse`] DTO.

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::enums::wallet::v1::WalletStatus;

/// The read-side view of a wallet — the `GET`/`POST` response body.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Schema)]
pub struct WalletResponse {
    /// The wallet's UUID, as a string.
    pub id: String,
    /// The human-facing account number (e.g. `"WAL-00001"`).
    #[serde(rename = "accountNumber")]
    pub account_number: String,
    /// The owner's display name.
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The ISO-4217 currency code.
    pub currency: String,
    /// The lifecycle status.
    pub status: WalletStatus,
    /// The optimistic-locking version (bumped on every write).
    pub version: i64,
}
