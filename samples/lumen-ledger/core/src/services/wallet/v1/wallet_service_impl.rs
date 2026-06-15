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

//! The [`WalletServiceImpl`] `@Service` bean.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use firefly::data::ReactiveCrudRepository;
use firefly::prelude::*;
use lumen_ledger_interfaces::{CreateWalletRequest, WalletResponse, WalletStatus};
use lumen_ledger_models::entities::wallet::v1::Wallet;
use lumen_ledger_models::repositories::wallet::v1::WalletRepository;
use uuid::Uuid;

use super::service_error::ServiceError;
use super::wallet_service::WalletService;
use crate::components::WalletNumberGenerator;
use crate::mappers::wallet::v1::WalletMapper;

/// The `@Service` implementation — a DI bean providing the
/// `dyn WalletService` port, autowiring the repository, mapper, and
/// number generator.
#[derive(Service)]
#[firefly(provides = "dyn WalletService")]
pub struct WalletServiceImpl {
    /// The persistence boundary (programmed against its
    /// `ReactiveCrudRepository` trait + derived queries).
    #[autowired]
    repository: Arc<WalletRepository>,
    /// The DTO ↔ entity mapper.
    #[autowired]
    mapper: Arc<WalletMapper>,
    /// The account-number `@Component`.
    #[autowired]
    numbers: Arc<WalletNumberGenerator>,
}

impl WalletServiceImpl {
    /// Loads a wallet, erroring `NotFound` when absent and
    /// `Validation` when it is not active.
    async fn load_active(&self, id: Uuid) -> Result<Wallet, ServiceError> {
        let wallet = self
            .repository
            .find_by_id(id)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?
            .ok_or(ServiceError::NotFound)?;
        if wallet.status != WalletStatus::Active.as_str() {
            return Err(ServiceError::Validation(format!(
                "wallet is {} and cannot transact",
                wallet.status
            )));
        }
        Ok(wallet)
    }

    /// Persists a wallet (UPSERT) and maps the stored row to a DTO.
    async fn persist(&self, wallet: Wallet) -> Result<WalletResponse, ServiceError> {
        let saved = self
            .repository
            .save(wallet)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?
            .ok_or_else(|| ServiceError::Backend("save returned no row".into()))?;
        Ok(self.mapper.to_response(&saved))
    }
}

#[async_trait]
impl WalletService for WalletServiceImpl {
    async fn create(&self, request: CreateWalletRequest) -> Result<WalletResponse, ServiceError> {
        if request.opening_balance < 0 {
            return Err(ServiceError::Validation(
                "opening balance cannot be negative".into(),
            ));
        }
        let now = Utc::now();
        let wallet = Wallet {
            id: Uuid::new_v4(),
            account_number: self.numbers.next_number(),
            owner: request.owner,
            balance: request.opening_balance,
            currency: request.currency,
            status: WalletStatus::Active.as_str().to_string(),
            version: 1,
            created_at: now,
            updated_at: now,
        };
        self.persist(wallet).await
    }

    async fn get(&self, id: Uuid) -> Result<WalletResponse, ServiceError> {
        let wallet = self
            .repository
            .find_by_id(id)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?
            .ok_or(ServiceError::NotFound)?;
        Ok(self.mapper.to_response(&wallet))
    }

    async fn list_by_owner(&self, owner: &str) -> Result<Vec<WalletResponse>, ServiceError> {
        let wallets = self
            .repository
            .find_by_owner(owner)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?;
        Ok(wallets.iter().map(|w| self.mapper.to_response(w)).collect())
    }

    async fn deposit(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError> {
        if amount <= 0 {
            return Err(ServiceError::Validation("amount must be positive".into()));
        }
        let mut wallet = self.load_active(id).await?;
        wallet.balance += amount;
        wallet.version += 1;
        wallet.updated_at = Utc::now();
        self.persist(wallet).await
    }

    async fn withdraw(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError> {
        if amount <= 0 {
            return Err(ServiceError::Validation("amount must be positive".into()));
        }
        let mut wallet = self.load_active(id).await?;
        if wallet.balance < amount {
            return Err(ServiceError::Validation("insufficient funds".into()));
        }
        wallet.balance -= amount;
        wallet.version += 1;
        wallet.updated_at = Utc::now();
        self.persist(wallet).await
    }
}
