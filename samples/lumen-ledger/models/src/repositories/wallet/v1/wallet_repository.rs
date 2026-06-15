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

use chrono::{DateTime, NaiveDateTime, Utc};
use firefly::data::{Auditor, DataError, Pageable, ReactiveCrudRepository, TableConfig};
use firefly::data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository};
use firefly::kernel::FireflyError;
use firefly::reactive::{Flux, Mono};
use lumen_ledger_interfaces::WalletStatus;
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

/// Reads one `wallets` row into a [`Wallet`] (the repository's `RowMapper`).
///
/// This is the single token→enum boundary (`@Enumerated(STRING)`): the stored
/// `status` token becomes the typed [`WalletStatus`]. The UUID and timestamps
/// are stored as text for portability across SQLite and PostgreSQL.
fn read_wallet(row: &AnyRow<'_>) -> Result<Wallet, FireflyError> {
    Ok(Wallet {
        id: Uuid::parse_str(&row.get_str("id")?)
            .map_err(|e| FireflyError::internal(format!("bad uuid: {e}")))?,
        account_number: row.get_str("account_number")?,
        owner: row.get_str("owner")?,
        balance: row.get_i64("balance")?,
        currency: row.get_str("currency")?,
        status: WalletStatus::from_token(&row.get_str("status")?),
        version: row.get_i64("version")?,
        created_at: read_dt(&row.get_str("created_at")?)?,
        updated_at: read_dt(&row.get_str("updated_at")?)?,
    })
}

/// Flattens a [`Wallet`] into the column values an INSERT/UPSERT binds — the
/// single enum→token boundary (`status.as_str()`).
///
/// `created_at` / `updated_at` are intentionally **not** stamped here: the
/// repository's [`Auditor`] owns them (`@CreatedDate` / `@LastModifiedDate`),
/// overwriting whatever the entity carries. `version` is emitted as the
/// **loaded** value so the `@Version` optimistic-locking guard can compare it.
fn write_wallet(w: &Wallet) -> Vec<ColumnValue> {
    vec![
        ColumnValue::new("id", w.id.to_string()),
        ColumnValue::new("account_number", w.account_number.clone()),
        ColumnValue::new("owner", w.owner.clone()),
        ColumnValue::new("balance", w.balance),
        ColumnValue::new("currency", w.currency.clone()),
        ColumnValue::new("status", w.status.as_str()),
        ColumnValue::new("version", w.version),
        // created_at re-sent so an UPDATE preserves it (the auditor only
        // stamps created_at on INSERT); the auditor overwrites updated_at.
        ColumnValue::new("created_at", w.created_at.to_rfc3339()),
    ]
}

/// Parses a stored timestamp, tolerating RFC 3339 (`T`-separated, what this
/// writer emits) and the space-separated form the sqlx SQLite/Postgres binders
/// produce for an auditor-stamped `DateTime<Utc>` — so an audit column written
/// by the framework round-trips back into the entity.
fn read_dt(s: &str) -> Result<DateTime<Utc>, FireflyError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%:z").map(|dt| dt.with_timezone(&Utc))
        })
        .or_else(|_| {
            NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f").map(|ndt| ndt.and_utc())
        })
        .map_err(|e| FireflyError::internal(format!("bad timestamp '{s}': {e}")))
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
    /// Builds the repository over an open [`Db`] pool, configured like a Spring
    /// Data entity: **`@Version` optimistic locking** on the `version` column (a
    /// stale write fails instead of silently winning) and a store **`Auditor`**
    /// that stamps `created_at` / `updated_at` (`@CreatedDate` /
    /// `@LastModifiedDate`).
    #[must_use]
    pub fn new(db: Db) -> Self {
        let cfg = TableConfig::new("wallets", "id", COLUMNS);
        let repo = SqlxReactiveRepository::new(db, cfg, read_wallet, write_wallet)
            .with_version_column("version")
            .with_auditor(Auditor::new());
        Self { repo }
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
    use firefly::data::{DataError, Pageable, ReactiveCrudRepository, RequestSort};
    use lumen_ledger_interfaces::WalletStatus;
    use uuid::Uuid;

    use crate::config::connect_and_migrate_url;
    use crate::entities::wallet::v1::Wallet;
    use crate::repositories::wallet::v1::WalletRepository;

    fn sample(owner: &str, status: WalletStatus) -> Wallet {
        Wallet {
            id: Uuid::new_v4(),
            account_number: format!("WAL-{owner}-{}", Uuid::new_v4()),
            owner: owner.into(),
            balance: 100,
            currency: "EUR".into(),
            status,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    async fn repo(cache: &str) -> WalletRepository {
        // Each test gets an isolated in-memory DB via a unique shared-cache name,
        // passed directly (no racing on the process-global DATABASE_URL env var).
        let url = format!("sqlite:file:{cache}?mode=memory&cache=shared");
        WalletRepository::new(connect_and_migrate_url(&url).await)
    }

    #[tokio::test]
    async fn repository_saves_and_runs_derived_queries() {
        let repo = repo("lumen_models_derived").await;

        let ada = sample("ada", WalletStatus::Active);
        let id = ada.id;
        repo.save(ada).await.expect("save ada");
        repo.save(sample("ada", WalletStatus::Frozen))
            .await
            .expect("save 2");
        repo.save(sample("bob", WalletStatus::Active))
            .await
            .expect("save bob");

        assert_eq!(repo.count().await.expect("count").unwrap_or(0), 3);

        // find_by_id round-trips the typed status enum.
        let found = repo.find_by_id(id).await.expect("find").expect("present");
        assert_eq!(found.owner, "ada");
        assert_eq!(found.status, WalletStatus::Active);

        assert_eq!(repo.find_by_owner("ada").await.expect("by owner").len(), 2);
        assert_eq!(repo.count_by_status("active").await.expect("count"), 2);
        assert_eq!(repo.count_by_status("frozen").await.expect("count"), 1);

        // Paged derived query (the only Pageable machinery in the sample).
        let page = repo
            .find_by_status(
                "active",
                Pageable::of(1, 1, RequestSort::of([])).expect("pageable"),
            )
            .await
            .expect("paged");
        assert_eq!(page.len(), 1, "page size 1 of the active wallets");

        assert!(repo
            .find_by_id(Uuid::new_v4())
            .await
            .expect("absent")
            .is_none());
    }

    #[tokio::test]
    async fn optimistic_locking_rejects_a_stale_write() {
        let repo = repo("lumen_models_optlock").await;
        let wallet = sample("ada", WalletStatus::Active);
        let id = wallet.id;
        repo.save(wallet).await.expect("insert");

        // Two readers load the same version.
        let a = repo.find_by_id(id).await.expect("load a").expect("present");
        let b = repo.find_by_id(id).await.expect("load b").expect("present");

        // First writer wins (the store bumps version).
        let mut a = a;
        a.balance += 10;
        repo.save(a).await.expect("first write wins");

        // Second writer holds the stale version → @Version conflict.
        let mut b = b;
        b.balance += 20;
        let err = repo.save(b).await.expect_err("stale write must conflict");
        assert!(
            firefly::data_sqlx::is_optimistic_lock(&err),
            "stale @Version write is an optimistic-lock conflict, got: {err}"
        );
    }

    // Belt-and-suspenders: the audit columns round-trip through read_dt.
    #[tokio::test]
    async fn auditor_stamps_round_trip() -> Result<(), DataError> {
        let repo = repo("lumen_models_audit").await;
        let id = sample("ada", WalletStatus::Active).id;
        let w = Wallet {
            id,
            ..sample("ada", WalletStatus::Active)
        };
        repo.save(w)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))?;
        let loaded = repo
            .find_by_id(id)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))?;
        assert!(loaded.is_some(), "audit-stamped row reads back");
        Ok(())
    }
}
