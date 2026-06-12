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

//! The account **read-model repository** — a
//! [`firefly_data::ReactiveCrudRepository`] over [`AccountView`] rows
//! (Spring Data R2DBC's `ReactiveCrudRepository<T, ID>` analog).
//!
//! Two backends share the one trait object:
//!
//! - [`new_in_memory`] — a [`ReactiveMemoryRepository`], the default, used
//!   by every in-process test and a no-infra `cargo run`.
//! - [`new_postgres`] — a real
//!   [`PostgresReactiveRepository`](firefly_data::PostgresReactiveRepository)
//!   over `tokio-postgres` that **streams rows lazily** as a `Flux`,
//!   selected when `FIREFLY_TEST_POSTGRES_URL` is set.
//!
//! Both expose the same `Arc<dyn ReactiveCrudRepository<AccountView,
//! String>>`, so the [`projections`](crate::projections) runner and the
//! [`web`](crate::web) query path are oblivious to which is wired —
//! ports-and-adapters with a reactive surface.

use std::sync::Arc;

use firefly_data::{
    PostgresReactiveRepository, ReactiveCrudRepository, ReactiveMemoryRepository, TableConfig,
};
use firefly_kernel::FireflyError;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Row};

use crate::domain::AccountView;

/// The object-safe read-model repository every layer programs against.
pub type AccountRepository = Arc<dyn ReactiveCrudRepository<AccountView, String>>;

/// The Postgres table the read model projects into when the real backend is
/// wired. `DDL` creates it idempotently (the e2e test runs it on boot).
pub const TABLE: &str = "account_view";

/// The `CREATE TABLE IF NOT EXISTS` statement matching [`TABLE`] and the
/// [`AccountView`] columns. Applied by the Postgres-backed boot path so the
/// schema is self-provisioning for the sample (a real service would use a
/// `firefly-migrations` Flyway-style migration instead).
pub const DDL: &str = "CREATE TABLE IF NOT EXISTS \"account_view\" (\
     \"id\" TEXT PRIMARY KEY, \
     \"owner\" TEXT NOT NULL, \
     \"balance\" BIGINT NOT NULL, \
     \"version\" BIGINT NOT NULL)";

/// Returns the in-memory read-model repository — the default backend, keyed
/// by [`AccountView::id`]. Cheap to clone and `Send + Sync`, so the same
/// handle is shared by the projection runner and the HTTP query path.
pub fn new_in_memory() -> AccountRepository {
    Arc::new(ReactiveMemoryRepository::new(|v: &AccountView| {
        v.id.clone()
    }))
}

/// Builds the **real Postgres** read-model repository over a live
/// `tokio-postgres` [`Client`].
///
/// Reads ([`find_by_id`](ReactiveCrudRepository::find_by_id),
/// [`find_all`](ReactiveCrudRepository::find_all)) stream rows lazily as a
/// `Flux`; [`save`](ReactiveCrudRepository::save) upserts by id with a
/// `RETURNING` clause projecting exactly the [`AccountView`] columns. This
/// is the genuine cross-infra path exercised by the
/// `FIREFLY_TEST_POSTGRES_URL`-gated e2e test.
pub fn new_postgres(client: Arc<Client>) -> AccountRepository {
    Arc::new(PostgresReactiveRepository::new(
        client,
        TableConfig::new(TABLE, "id", ["id", "owner", "balance", "version"]),
        // RowMapper: decode (id, owner, balance, version).
        |row: &Row| {
            Ok(AccountView {
                id: row.try_get("id").map_err(map_pg)?,
                owner: row.try_get("owner").map_err(map_pg)?,
                balance: row.try_get("balance").map_err(map_pg)?,
                version: row.try_get("version").map_err(map_pg)?,
            })
        },
        // inserter: upsert RETURNING the projected columns (idempotent
        // re-projection — a replayed event re-writes the same row).
        |v: &AccountView| {
            (
                "INSERT INTO \"account_view\" (\"id\", \"owner\", \"balance\", \"version\") \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (\"id\") DO UPDATE SET \
                   \"owner\" = EXCLUDED.\"owner\", \
                   \"balance\" = EXCLUDED.\"balance\", \
                   \"version\" = EXCLUDED.\"version\" \
                 RETURNING \"id\", \"owner\", \"balance\", \"version\""
                    .to_string(),
                vec![
                    Box::new(v.id.clone()) as Box<dyn ToSql + Sync + Send>,
                    Box::new(v.owner.clone()) as Box<dyn ToSql + Sync + Send>,
                    Box::new(v.balance) as Box<dyn ToSql + Sync + Send>,
                    Box::new(v.version) as Box<dyn ToSql + Sync + Send>,
                ],
            )
        },
    ))
}

fn map_pg(e: tokio_postgres::Error) -> FireflyError {
    FireflyError::internal(format!("reactive-banking/repository: postgres: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, balance: i64) -> AccountView {
        AccountView {
            id: id.into(),
            owner: "alice".into(),
            balance,
            version: 1,
        }
    }

    #[tokio::test]
    async fn in_memory_save_then_find_streams_back() {
        let repo = new_in_memory();
        repo.save(view("a1", 100)).block().await.unwrap();
        let got = repo.find_by_id("a1".into()).block().await.unwrap();
        assert_eq!(got, Some(view("a1", 100)));
        assert_eq!(repo.count().block().await.unwrap(), Some(1));
    }

    #[tokio::test]
    async fn in_memory_save_is_upsert() {
        let repo = new_in_memory();
        repo.save(view("a1", 100)).block().await.unwrap();
        repo.save(view("a1", 250)).block().await.unwrap();
        let got = repo.find_by_id("a1".into()).block().await.unwrap().unwrap();
        assert_eq!(got.balance, 250);
        assert_eq!(repo.count().block().await.unwrap(), Some(1));
    }

    #[tokio::test]
    async fn missing_account_is_empty_mono() {
        let repo = new_in_memory();
        assert_eq!(repo.find_by_id("ghost".into()).block().await.unwrap(), None);
    }

    #[test]
    fn ddl_targets_the_table() {
        assert!(DDL.contains(TABLE));
        assert!(DDL.contains("\"balance\" BIGINT"));
    }
}
