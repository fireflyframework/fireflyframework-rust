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

//! The [`WalletStatus`] domain enum.

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

/// The lifecycle state of a wallet. Serialises as a lowercase string
/// (`"active"`, `"frozen"`, `"closed"`) and is emitted into the
/// OpenAPI document as a `string` enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
#[serde(rename_all = "lowercase")]
pub enum WalletStatus {
    /// Open and able to transact.
    #[default]
    Active,
    /// Temporarily blocked from debits/credits.
    Frozen,
    /// Permanently closed.
    Closed,
}

impl WalletStatus {
    /// The lowercase wire/storage token for this status.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Frozen => "frozen",
            Self::Closed => "closed",
        }
    }

    /// Parses a stored status token back into the enum, defaulting an
    /// unrecognised value to [`WalletStatus::Active`] (a forward-
    /// compatible read that never panics on legacy rows).
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "frozen" => Self::Frozen,
            "closed" => Self::Closed,
            _ => Self::Active,
        }
    }
}

impl std::fmt::Display for WalletStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
