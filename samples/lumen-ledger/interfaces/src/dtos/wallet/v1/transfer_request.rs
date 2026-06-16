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

//! The [`TransferRequest`] DTO.

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

/// `POST /…/{id}/transfer` body — move `amount` minor units from the path
/// wallet (the source) to the `to` wallet (the destination).
///
/// Both constraints are enforced at the web edge through the `Valid<…>`
/// extractor (RFC 9457 `422` before the service runs): `to` must be present
/// (`#[validate(not_empty)]`) and `amount` must be positive
/// (`#[validate(range(min = 1))]`, the Spring `@Min(1)` analog). The `to` value
/// is a wallet id string, matching the `id` the client received in a
/// [`WalletResponse`](crate::WalletResponse).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Schema, Validate)]
pub struct TransferRequest {
    /// The destination wallet id (the `id` string from a `WalletResponse`).
    #[validate(not_empty)]
    pub to: String,
    /// The amount to move, in minor units (cents); must be `>= 1`.
    #[validate(range(min = 1))]
    pub amount: i64,
}
