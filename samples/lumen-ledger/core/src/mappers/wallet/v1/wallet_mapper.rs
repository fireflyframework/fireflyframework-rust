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

//! The [`WalletMapper`] `@Mapper`.

use firefly::prelude::*;
use lumen_ledger_interfaces::{WalletResponse, WalletStatus};
use lumen_ledger_models::entities::wallet::v1::Wallet;

/// Translates the persistence [`Wallet`] entity into the public
/// [`WalletResponse`] DTO — MapStruct's `@Mapper`, realised as a DI
/// `@Component` bean the service autowires.
///
/// A hand-written mapper bean (rather than `#[derive(Mapper)]`)
/// because the two sides live in different crates: Rust's orphan rule
/// forbids `impl From<Wallet> for WalletResponse` in `-core` (both
/// types are foreign here), so the cross-layer mapper is expressed as
/// a bean with methods — exactly the shape MapStruct generates.
#[derive(Component, Default)]
pub struct WalletMapper;

impl WalletMapper {
    /// Entity → response DTO (the status token becomes the typed
    /// [`WalletStatus`] enum; the UUID becomes a string).
    #[must_use]
    pub fn to_response(&self, wallet: &Wallet) -> WalletResponse {
        WalletResponse {
            id: wallet.id.to_string(),
            account_number: wallet.account_number.clone(),
            owner: wallet.owner.clone(),
            balance: wallet.balance,
            currency: wallet.currency.clone(),
            status: WalletStatus::from_token(&wallet.status),
            version: wallet.version,
        }
    }
}
