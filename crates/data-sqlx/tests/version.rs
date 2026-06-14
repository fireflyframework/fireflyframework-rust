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

//! Optimistic-locking (`@Version`) tests for the sqlx adapter on SQLite — the
//! lost-update prevention the parity audit flagged as missing.

#![cfg(feature = "sqlite")]

use firefly_data::{DataError, Repository, TableConfig};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone, PartialEq)]
struct Doc {
    id: i64,
    name: String,
    version: i64,
}

fn map_doc(row: &AnyRow<'_>) -> Result<Doc, FireflyError> {
    Ok(Doc {
        id: row.get_i64("id")?,
        name: row.get_str("name")?,
        version: row.get_i64("version")?,
    })
}

fn write_doc(d: &Doc) -> Vec<ColumnValue> {
    vec![
        ColumnValue::new("id", d.id),
        ColumnValue::new("name", d.name.clone()),
        ColumnValue::new("version", d.version),
    ]
}

async fn repo() -> SqlxRepository<Doc, i64> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    sqlx::query(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("create table");
    let cfg = TableConfig::new("docs", "id", ["id", "name", "version"]);
    SqlxRepository::new(Db::Sqlite(pool), cfg, map_doc, write_doc).with_version_column("version")
}

#[tokio::test]
async fn version_bumps_on_update_and_rejects_stale_write() {
    let repo = repo().await;

    // Initial insert at version 0.
    let inserted = repo
        .save(Doc {
            id: 1,
            name: "a".into(),
            version: 0,
        })
        .await
        .expect("insert");
    assert_eq!(inserted.version, 0);

    // Writer A loaded version 0 and saves: the guard matches, the row updates
    // and the version is bumped to 1.
    let updated = repo
        .save(Doc {
            id: 1,
            name: "a2".into(),
            version: 0,
        })
        .await
        .expect("matching-version update");
    assert_eq!(updated.version, 1, "version bumped on conflict-update");
    assert_eq!(updated.name, "a2");

    // Writer B also loaded version 0 (now stale: the DB is at 1). Its save must
    // be rejected with an optimistic-lock conflict, not silently win.
    let stale = repo
        .save(Doc {
            id: 1,
            name: "b2".into(),
            version: 0,
        })
        .await;
    assert_eq!(stale, Err(DataError::OptimisticLock));

    // The row was not clobbered by the stale write.
    let current = repo.find_by_id(&1).await.expect("find");
    assert_eq!(current.name, "a2");
    assert_eq!(current.version, 1);
}
