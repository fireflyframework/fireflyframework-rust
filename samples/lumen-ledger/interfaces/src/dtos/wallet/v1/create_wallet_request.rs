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

//! The [`CreateWalletRequest`] DTO.

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

/// `POST /api/v1/wallets` body — open a new wallet. `owner` and
/// `currency` are required (`#[firefly(validate)]`): a blank value
/// fails validation at the web edge and renders RFC 9457 `422`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Schema, Validate)]
pub struct CreateWalletRequest {
    /// The wallet owner's display name — required, non-blank.
    #[validate(not_empty, length(max = 120))]
    pub owner: String,
    /// The ISO-4217 currency code (e.g. `"EUR"`) — required, 3 chars.
    #[validate(not_empty, length(min = 3, max = 3))]
    pub currency: String,
    /// The opening balance in minor units (cents); defaults to `0`.
    #[serde(default, rename = "openingBalance")]
    pub opening_balance: i64,
}
