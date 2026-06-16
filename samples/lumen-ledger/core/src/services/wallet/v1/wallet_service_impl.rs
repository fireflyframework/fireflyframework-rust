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
use firefly::data::{Page, Pageable, ReactiveCrudRepository};
use firefly::data_sqlx::{Db, SqlxTransactionManager};
use firefly::prelude::*;
use firefly::transactional::TransactionManager;
use lumen_ledger_interfaces::{CreateWalletRequest, WalletResponse, WalletStatus};
use lumen_ledger_models::entities::wallet::v1::Wallet;
use lumen_ledger_models::is_optimistic_lock;
use lumen_ledger_models::repositories::wallet::v1::WalletRepository;
use uuid::Uuid;

use super::service_error::ServiceError;
use super::wallet_service::WalletService;
use crate::components::WalletNumberGenerator;
use crate::mappers::wallet::v1::WalletMapper;

/// The `@Service` implementation — a DI bean providing the `dyn WalletService`
/// port, autowiring the repository, mapper, and number generator.
///
/// `deposit` / `withdraw` are read-modify-writes guarded by the repository's
/// **`@Version` optimistic locking**: a concurrent change cannot be silently
/// lost — a stale write surfaces as a `409` conflict (the
/// `OptimisticLockingFailureException` analog).
#[derive(Service)]
#[firefly(provides = "dyn WalletService")]
pub struct WalletServiceImpl {
    /// The persistence boundary (programmed against its `ReactiveCrudRepository`
    /// trait + derived queries).
    #[autowired]
    repository: Arc<WalletRepository>,
    /// The DTO ↔ entity mapper.
    #[autowired]
    mapper: Arc<WalletMapper>,
    /// The account-number `@Component`.
    #[autowired]
    numbers: Arc<WalletNumberGenerator>,
    /// The datasource (Spring Boot's `DataSource`), autowired so the service can
    /// build its **own** transaction manager for the atomic `transfer` —
    /// see [`tx_manager`](Self::tx_manager).
    #[autowired]
    db: Arc<Db>,
}

impl WalletServiceImpl {
    /// This service's **own** transaction manager, built from the autowired
    /// datasource. `#[transactional(manager = "self.tx_manager()")]` evaluates it
    /// per call, so `transfer` runs against an explicit manager instead of the
    /// process-global registry — Spring's `@Transactional("txManager")`. That
    /// keeps a multi-datasource service (and the per-test isolated databases of
    /// this sample's suite) correct, where one process-global manager could not.
    fn tx_manager(&self) -> Arc<dyn TransactionManager> {
        Arc::new(SqlxTransactionManager::new((*self.db).clone()))
    }

    /// Atomically moves `amount` minor units from `from` to `to` — both wallets
    /// must be active and share a currency. The debit and credit run inside
    /// **one transaction** bound to this service's manager: every precondition
    /// (positive amount, distinct active wallets, matching currency, sufficient
    /// funds) is checked **before** the source is debited, so a rejected transfer
    /// moves no money; and if the *credit* fails after the debit (a backend error
    /// or a stale `@Version` write on the destination), the transaction rolls the
    /// debit back — the partial-write protection proven end-to-end by
    /// `firefly-data-sqlx`'s `tests/transactional.rs`. Returns the updated source.
    #[firefly::transactional(manager = "self.tx_manager()")]
    async fn transfer_tx(
        &self,
        from: Uuid,
        to: Uuid,
        amount: i64,
    ) -> Result<WalletResponse, ServiceError> {
        if amount <= 0 {
            return Err(ServiceError::Validation(
                "transfer amount must be positive".into(),
            ));
        }
        if from == to {
            return Err(ServiceError::Validation(
                "cannot transfer to the same wallet".into(),
            ));
        }
        let mut source = self.load_active(from).await?;
        let mut dest = self.load_active(to).await?;
        // A ledger must not move value across currencies — that would create or
        // destroy money. Both wallets carry an ISO-4217 code; they must match.
        if source.currency != dest.currency {
            return Err(ServiceError::Validation(format!(
                "currency mismatch: cannot transfer {} to {}",
                source.currency, dest.currency
            )));
        }
        if source.balance < amount {
            return Err(ServiceError::Validation("insufficient funds".into()));
        }

        // Debit the source, then credit the destination — checked arithmetic so a
        // ledger overflow is a domain error, never a silent wrap. If the credit
        // fails after the debit, the transaction rolls the debit back.
        source.balance = source
            .balance
            .checked_sub(amount)
            .ok_or_else(|| ServiceError::Validation("balance underflow".into()))?;
        let saved_source = self.persist(source).await?;
        dest.balance = dest
            .balance
            .checked_add(amount)
            .ok_or_else(|| ServiceError::Validation("balance overflow".into()))?;
        self.persist(dest).await?;
        Ok(saved_source)
    }

    /// Loads a wallet, erroring `NotFound` when absent and `Validation` when it
    /// is not active (so a frozen/closed wallet cannot transact).
    async fn load_active(&self, id: Uuid) -> Result<Wallet, ServiceError> {
        let wallet = self
            .repository
            .find_by_id(id)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?
            .ok_or(ServiceError::NotFound)?;
        if wallet.status != WalletStatus::Active {
            return Err(ServiceError::Validation(format!(
                "wallet is {} and cannot transact",
                wallet.status
            )));
        }
        Ok(wallet)
    }

    /// Persists a wallet (UPSERT) and maps the stored row to a DTO. A stale
    /// `@Version` write is mapped to [`ServiceError::Conflict`] (409).
    async fn persist(&self, wallet: Wallet) -> Result<WalletResponse, ServiceError> {
        let saved = self
            .repository
            .save(wallet)
            .await
            .map_err(|e| {
                if is_optimistic_lock(&e) {
                    ServiceError::Conflict("wallet was modified concurrently; retry".into())
                } else {
                    ServiceError::Backend(e.to_string())
                }
            })?
            .ok_or_else(|| ServiceError::Backend("save returned no row".into()))?;
        Ok(self.mapper.to_response(&saved))
    }
}

#[async_trait]
impl WalletService for WalletServiceImpl {
    async fn create(&self, request: CreateWalletRequest) -> Result<WalletResponse, ServiceError> {
        // opening_balance is validated non-negative at the web edge; the entity
        // is created Active, and the store stamps version/timestamps.
        let now = chrono::Utc::now();
        let wallet = Wallet {
            id: Uuid::new_v4(),
            account_number: self.numbers.next_number(),
            owner: request.owner,
            balance: request.opening_balance,
            currency: request.currency,
            status: WalletStatus::Active,
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

    async fn list_by_status(
        &self,
        status: WalletStatus,
        pageable: Pageable,
    ) -> Result<Page<WalletResponse>, ServiceError> {
        let token = status.as_str();
        let (page, size) = (pageable.page, pageable.size);
        let rows = self
            .repository
            .find_by_status(token, pageable)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?;
        let total = self
            .repository
            .count_by_status(token)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))? as u64;
        let content = rows.iter().map(|w| self.mapper.to_response(w)).collect();
        Ok(Page::new(content, page, size, total))
    }

    async fn deposit(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError> {
        let mut wallet = self.load_active(id).await?;
        wallet.balance += amount; // version + updated_at are stamped by the store
        self.persist(wallet).await
    }

    async fn withdraw(&self, id: Uuid, amount: i64) -> Result<WalletResponse, ServiceError> {
        let mut wallet = self.load_active(id).await?;
        if wallet.balance < amount {
            return Err(ServiceError::Validation("insufficient funds".into()));
        }
        wallet.balance -= amount;
        self.persist(wallet).await
    }

    async fn transfer(
        &self,
        from: Uuid,
        to: Uuid,
        amount: i64,
    ) -> Result<WalletResponse, ServiceError> {
        // The `#[transactional]` boundary lives on the inherent `transfer_tx`
        // (an async-trait method can't carry the attribute cleanly); the trait
        // method just delegates.
        self.transfer_tx(from, to, amount).await
    }

    async fn set_status(
        &self,
        id: Uuid,
        status: WalletStatus,
    ) -> Result<WalletResponse, ServiceError> {
        let mut wallet = self
            .repository
            .find_by_id(id)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?
            .ok_or(ServiceError::NotFound)?;
        wallet.status = status;
        self.persist(wallet).await
    }

    async fn delete(&self, id: Uuid) -> Result<(), ServiceError> {
        self.repository
            .delete_by_id(id)
            .await
            .map_err(|e| ServiceError::Backend(e.to_string()))?;
        Ok(())
    }
}
