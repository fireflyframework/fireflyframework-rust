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

//! The [`WalletRepository`] — a real sqlx repository with derived queries.

use chrono::{DateTime, Utc};
use firefly::data::{DataError, Pageable, ReactiveCrudRepository, TableConfig};
use firefly::data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository};
use firefly::kernel::FireflyError;
use firefly::reactive::{Flux, Mono};
use uuid::Uuid;

use crate::entities::wallet::v1::Wallet;

/// The ordered column list of the `wallets` table — shared by the
/// [`TableConfig`] and the row reader/writer below.
const COLUMNS: [&str; 9] = [
    "id",
    "account_number",
    "owner",
    "balance",
    "currency",
    "status",
    "version",
    "created_at",
    "updated_at",
];

/// Reads one `wallets` row into a [`Wallet`] (the repository's
/// `RowMapper`). Timestamps and the UUID are stored as text for
/// portability across SQLite and PostgreSQL.
fn read_wallet(row: &AnyRow<'_>) -> Result<Wallet, FireflyError> {
    Ok(Wallet {
        id: Uuid::parse_str(&row.get_str("id")?)
            .map_err(|e| FireflyError::internal(format!("bad uuid: {e}")))?,
        account_number: row.get_str("account_number")?,
        owner: row.get_str("owner")?,
        balance: row.get_i64("balance")?,
        currency: row.get_str("currency")?,
        status: row.get_str("status")?,
        version: row.get_i64("version")?,
        created_at: read_dt(&row.get_str("created_at")?)?,
        updated_at: read_dt(&row.get_str("updated_at")?)?,
    })
}

/// Flattens a [`Wallet`] into the column values an INSERT/UPSERT binds.
fn write_wallet(w: &Wallet) -> Vec<ColumnValue> {
    vec![
        ColumnValue::new("id", w.id.to_string()),
        ColumnValue::new("account_number", w.account_number.clone()),
        ColumnValue::new("owner", w.owner.clone()),
        ColumnValue::new("balance", w.balance),
        ColumnValue::new("currency", w.currency.clone()),
        ColumnValue::new("status", w.status.clone()),
        ColumnValue::new("version", w.version),
        ColumnValue::new("created_at", w.created_at.to_rfc3339()),
        ColumnValue::new("updated_at", w.updated_at.to_rfc3339()),
    ]
}

fn read_dt(s: &str) -> Result<DateTime<Utc>, FireflyError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| FireflyError::internal(format!("bad timestamp: {e}")))
}

/// The wallet persistence boundary — Spring Data's `WalletRepository`.
///
/// **Extends the framework's
/// [`ReactiveCrudRepository`]** (`impl` below), so it inherits the
/// canonical reactive CRUD surface — `find_all`, `find_by_id`, `save`,
/// `delete_by_id`, `count`, … returning `Mono`/`Flux` — exactly like a
/// Spring Data `interface WalletRepository extends
/// ReactiveCrudRepository<Wallet, UUID>`. On top of that it carries
/// the `#[firefly::repository]` derived queries. The service layer
/// autowires it as `Arc<WalletRepository>` and programs against the
/// `ReactiveCrudRepository` trait + the derived queries.
///
/// The key type is [`Uuid`] — the framework's `SqlKey` lets any
/// serializable type be a repository key (Java's unbounded `ID`
/// generic), binding the UUID as its canonical string against the
/// text `id` column.
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}

impl WalletRepository {
    /// Builds the repository over an open [`Db`] pool.
    #[must_use]
    pub fn new(db: Db) -> Self {
        let cfg = TableConfig::new("wallets", "id", COLUMNS);
        Self {
            repo: SqlxReactiveRepository::new(db, cfg, read_wallet, write_wallet),
        }
    }

    /// The accessor the `#[firefly::repository]` derived queries call
    /// to reach the underlying sqlx repository.
    fn repository(&self) -> &SqlxReactiveRepository<Wallet, Uuid> {
        &self.repo
    }
}

// "extends ReactiveCrudRepository<Wallet, UUID>" — the canonical CRUD
// surface, delegated to the inner sqlx repository. Programming the
// service against this trait (rather than bespoke methods) is the
// whole point of the framework's repository abstraction.
impl ReactiveCrudRepository<Wallet, Uuid> for WalletRepository {
    fn find_all(&self) -> Flux<Wallet> {
        self.repo.find_all()
    }
    fn find_all_by_id(&self, ids: Vec<Uuid>) -> Flux<Wallet> {
        self.repo.find_all_by_id(ids)
    }
    fn find_by_id(&self, id: Uuid) -> Mono<Wallet> {
        self.repo.find_by_id(id)
    }
    fn exists_by_id(&self, id: Uuid) -> Mono<bool> {
        self.repo.exists_by_id(id)
    }
    fn save(&self, entity: Wallet) -> Mono<Wallet> {
        self.repo.save(entity)
    }
    fn save_all(&self, entities: Vec<Wallet>) -> Flux<Wallet> {
        self.repo.save_all(entities)
    }
    fn delete_by_id(&self, id: Uuid) -> Mono<()> {
        self.repo.delete_by_id(id)
    }
    fn delete_all(&self) -> Mono<()> {
        self.repo.delete_all()
    }
    fn count(&self) -> Mono<u64> {
        self.repo.count()
    }
}

// The declarative derived-query surface — method bodies generated by
// `#[firefly::repository]` from each method name (Spring Data style).
#[firefly::repository]
impl WalletRepository {
    /// `SELECT … WHERE owner = ?` — every wallet of one owner.
    pub async fn find_by_owner(&self, owner: &str) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }

    /// `SELECT COUNT(*) WHERE status = ?`.
    pub async fn count_by_status(&self, status: &str) -> Result<i64, DataError> {
        unimplemented!()
    }

    /// Paged `SELECT … WHERE status = ?` (ORDER BY/LIMIT/OFFSET from
    /// the trailing [`Pageable`]).
    pub async fn find_by_status(
        &self,
        status: &str,
        page: Pageable,
    ) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use firefly::data::ReactiveCrudRepository;
    use uuid::Uuid;

    use crate::config::connect_and_migrate;
    use crate::entities::wallet::v1::Wallet;
    use crate::repositories::wallet::v1::WalletRepository;

    fn sample(owner: &str, status: &str) -> Wallet {
        Wallet {
            id: Uuid::new_v4(),
            account_number: format!("WAL-{owner}"),
            owner: owner.into(),
            balance: 100,
            currency: "EUR".into(),
            status: status.into(),
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn repository_saves_and_runs_derived_queries() {
        // A private in-memory database for this test (unique cache name).
        std::env::set_var(
            "DATABASE_URL",
            "sqlite:file:lumen_models_test?mode=memory&cache=shared",
        );
        let repo = WalletRepository::new(connect_and_migrate().await);
        std::env::remove_var("DATABASE_URL");

        let ada = sample("ada", "active");
        let id = ada.id;
        // CRUD from the `ReactiveCrudRepository` trait (Mono/Flux), awaited.
        repo.save(ada).await.expect("save ada");
        repo.save(sample("ada", "frozen")).await.expect("save 2");
        repo.save(sample("bob", "active")).await.expect("save bob");

        // count() (trait) sees all three.
        assert_eq!(repo.count().await.expect("count").unwrap_or(0), 3);

        // find_by_id (trait) round-trips — keyed by the Uuid directly.
        let found = repo.find_by_id(id).await.expect("find").expect("present");
        assert_eq!(found.owner, "ada");

        // Derived query: find_by_owner.
        let ada_wallets = repo.find_by_owner("ada").await.expect("by owner");
        assert_eq!(ada_wallets.len(), 2, "ada owns two wallets");

        // Derived query: count_by_status.
        assert_eq!(repo.count_by_status("active").await.expect("count"), 2);
        assert_eq!(repo.count_by_status("frozen").await.expect("count"), 1);

        // Absent id → Ok(None) (trait find_by_id, empty Mono).
        assert!(repo
            .find_by_id(Uuid::new_v4())
            .await
            .expect("absent")
            .is_none());
    }
}
