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

//! The [`WalletFilter`] query-criteria DTO.

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::WalletStatus;

/// `GET /…/wallets/search?owner=&currency=&status=&minBalance=&maxBalance=` —
/// optional filter criteria, **AND**-combined. Every field is optional; an
/// omitted field adds no constraint. Bound straight from the query string, so
/// each field renders as an `in: query` parameter in Swagger UI / ReDoc, and the
/// `@Service` translates the set into a framework
/// [`Specification`](firefly::data::Specification) run by the repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Schema)]
pub struct WalletFilter {
    /// Exact owner match.
    #[serde(default)]
    pub owner: Option<String>,
    /// Exact ISO-4217 currency match (e.g. `EUR`).
    #[serde(default)]
    pub currency: Option<String>,
    /// Exact lifecycle status (`active` / `frozen` / `closed`).
    #[serde(default)]
    pub status: Option<WalletStatus>,
    /// Minimum balance, inclusive, in minor units (cents).
    #[serde(default, rename = "minBalance")]
    pub min_balance: Option<i64>,
    /// Maximum balance, inclusive, in minor units (cents).
    #[serde(default, rename = "maxBalance")]
    pub max_balance: Option<i64>,
}
