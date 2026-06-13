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

//! Shared test fixtures + a backend-neutral test suite, so the SQLite tests
//! (bare-machine) and the env-gated Postgres / MySQL round-trips exercise
//! the **same** CRUD / specification / pageable / auditing / soft-delete
//! assertions against one codebase.

#![allow(dead_code)]

use std::sync::Arc;

use firefly_data::{
    Auditor, Direction, Op, Order, Pageable, Predicate, ReactiveCrudRepository,
    ReactiveSpecificationRepository, Repository, RequestSort, SoftDeletePolicy, Specification,
    TableConfig, UserProvider,
};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository, SqlxRepository};
use firefly_kernel::FireflyError;

/// The test entity: an id, a name, a numeric score, an "active" flag, plus
/// the standard audit + soft-delete columns.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: String,
    pub name: String,
    pub score: i64,
    pub active: bool,
}

impl User {
    pub fn new(id: &str, name: &str, score: i64, active: bool) -> Self {
        User {
            id: id.into(),
            name: name.into(),
            score,
            active,
        }
    }
}

/// The columns this test table projects for reads.
pub fn columns() -> Vec<&'static str> {
    vec![
        "id",
        "name",
        "score",
        "active",
        "created_at",
        "updated_at",
        "created_by",
        "updated_by",
        "deleted_at",
    ]
}

pub fn table_config() -> TableConfig {
    TableConfig::new("users", "id", columns())
}

/// Backend-neutral row mapper for [`User`] — written once against
/// [`AnyRow`], runs on Postgres / MySQL / SQLite.
pub fn map_user(row: &AnyRow<'_>) -> Result<User, FireflyError> {
    Ok(User {
        id: row.get_str("id")?,
        name: row.get_str("name")?,
        score: row.get_i64("score")?,
        active: row.get_bool("active")?,
    })
}

/// Row writer for [`User`] (no audit columns set by the writer — the
/// repository's auditor fills them in).
pub fn write_user(u: &User) -> Vec<ColumnValue> {
    vec![
        ColumnValue::new("id", u.id.clone()),
        ColumnValue::new("name", u.name.clone()),
        ColumnValue::new("score", u.score),
        ColumnValue::new("active", u.active),
    ]
}

/// Builds a fresh reactive repository over `db` with no auditing / soft
/// delete.
pub fn reactive_repo(db: Db) -> SqlxReactiveRepository<User, String> {
    SqlxReactiveRepository::new(db, table_config(), map_user, write_user)
}

/// Builds a fresh blocking repository over `db`.
pub fn blocking_repo(db: Db) -> SqlxRepository<User, String> {
    SqlxRepository::new(db, table_config(), map_user, write_user)
}

/// A fixed-user provider for deterministic audit assertions.
pub fn fixed_user_provider(user: &str) -> UserProvider {
    let user = user.to_string();
    Arc::new(move || Some(user.clone()))
}

/// The `CREATE TABLE` DDL for `dialect_kind`, covering the boolean,
/// timestamp, and nullable columns the suite exercises. Booleans are stored
/// as the backend's native boolean (`BOOLEAN` / `TINYINT(1)`); timestamps as
/// `TIMESTAMP` text/instant the chrono decoder reads.
pub fn create_table_ddl(backend: firefly_data_sqlx::Backend) -> Vec<String> {
    use firefly_data_sqlx::Backend;
    match backend {
        Backend::Postgres => vec![
            r#"DROP TABLE IF EXISTS "users""#.to_string(),
            r#"CREATE TABLE "users" (
                "id" TEXT PRIMARY KEY,
                "name" TEXT NOT NULL,
                "score" BIGINT NOT NULL,
                "active" BOOLEAN NOT NULL,
                "created_at" TIMESTAMPTZ,
                "updated_at" TIMESTAMPTZ,
                "created_by" TEXT,
                "updated_by" TEXT,
                "deleted_at" TIMESTAMPTZ
            )"#
            .to_string(),
        ],
        Backend::MySql => vec![
            "DROP TABLE IF EXISTS `users`".to_string(),
            "CREATE TABLE `users` (
                `id` VARCHAR(64) PRIMARY KEY,
                `name` VARCHAR(255) NOT NULL,
                `score` BIGINT NOT NULL,
                `active` BOOLEAN NOT NULL,
                `created_at` TIMESTAMP NULL,
                `updated_at` TIMESTAMP NULL,
                `created_by` VARCHAR(255) NULL,
                `updated_by` VARCHAR(255) NULL,
                `deleted_at` TIMESTAMP NULL
            )"
            .to_string(),
        ],
        Backend::Sqlite => vec![
            r#"DROP TABLE IF EXISTS "users""#.to_string(),
            r#"CREATE TABLE "users" (
                "id" TEXT PRIMARY KEY,
                "name" TEXT NOT NULL,
                "score" BIGINT NOT NULL,
                "active" BOOLEAN NOT NULL,
                "created_at" TEXT,
                "updated_at" TEXT,
                "created_by" TEXT,
                "updated_by" TEXT,
                "deleted_at" TEXT
            )"#
            .to_string(),
        ],
    }
}

/// Runs the DDL for `db` against the pool.
pub async fn create_table(db: &Db) {
    let backend = db.backend();
    for stmt in create_table_ddl(backend) {
        match db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => {
                sqlx::query(&stmt)
                    .execute(pool)
                    .await
                    .expect("create table");
            }
            #[cfg(feature = "mysql")]
            Db::MySql(pool) => {
                sqlx::query(&stmt)
                    .execute(pool)
                    .await
                    .expect("create table");
            }
            #[cfg(feature = "sqlite")]
            Db::Sqlite(pool) => {
                sqlx::query(&stmt)
                    .execute(pool)
                    .await
                    .expect("create table");
            }
        }
    }
}

/// The complete backend-neutral suite: full CRUD, specification, pageable,
/// auditing, and soft-delete. Every backend's test entry point creates the
/// table and calls this against a freshly-built [`Db`].
pub async fn run_full_suite(db: Db) {
    create_table(&db).await;
    crud_round_trip(db.clone()).await;

    create_table(&db).await;
    specification_queries(db.clone()).await;

    create_table(&db).await;
    pageable_queries(db.clone()).await;

    create_table(&db).await;
    blocking_filter_and_page(db.clone()).await;

    create_table(&db).await;
    auditing_stamps_on_write(db.clone()).await;

    create_table(&db).await;
    soft_delete_hides_rows(db.clone()).await;

    create_table(&db).await;
    save_after_soft_delete_resurrects(db.clone()).await;

    create_table(&db).await;
    rfc3339_text_value_round_trips(db.clone()).await;
}

/// Full reactive CRUD: save / find_by_id / exists / save_all / find_all /
/// find_all_by_id / count / delete_by_id / delete_all.
pub async fn crud_round_trip(db: Db) {
    let repo = reactive_repo(db);

    // Empty store.
    assert_eq!(repo.find_by_id("x".into()).block().await.unwrap(), None);
    assert!(!repo
        .exists_by_id("x".into())
        .block()
        .await
        .unwrap()
        .unwrap());
    assert_eq!(repo.count().block().await.unwrap(), Some(0));

    // save -> persisted value back.
    let saved = repo
        .save(User::new("u1", "alice", 10, true))
        .block()
        .await
        .unwrap();
    assert_eq!(saved, Some(User::new("u1", "alice", 10, true)));

    // find_by_id hit + exists.
    assert_eq!(
        repo.find_by_id("u1".into()).block().await.unwrap(),
        Some(User::new("u1", "alice", 10, true))
    );
    assert!(repo
        .exists_by_id("u1".into())
        .block()
        .await
        .unwrap()
        .unwrap());

    // save is an upsert.
    repo.save(User::new("u1", "alice2", 11, false))
        .block()
        .await
        .unwrap();
    assert_eq!(
        repo.find_by_id("u1".into()).block().await.unwrap(),
        Some(User::new("u1", "alice2", 11, false))
    );
    assert_eq!(repo.count().block().await.unwrap(), Some(1));

    // save_all streams the persisted rows.
    let mut saved_all = repo
        .save_all(vec![
            User::new("u2", "bob", 20, true),
            User::new("u3", "carol", 30, true),
        ])
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    saved_all.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        saved_all,
        vec![
            User::new("u2", "bob", 20, true),
            User::new("u3", "carol", 30, true),
        ]
    );
    assert_eq!(repo.count().block().await.unwrap(), Some(3));

    // find_all streams everything.
    let mut all = repo
        .find_all()
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    all.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(all.len(), 3);

    // find_all_by_id selects a subset, skipping the missing id.
    let mut subset = repo
        .find_all_by_id(vec!["u1".into(), "u3".into(), "ghost".into()])
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    subset.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(subset.len(), 2);
    assert_eq!(subset[0].id, "u1");
    assert_eq!(subset[1].id, "u3");

    // delete_by_id (physical) then gone.
    repo.delete_by_id("u1".into()).block().await.unwrap();
    assert_eq!(repo.find_by_id("u1".into()).block().await.unwrap(), None);
    assert_eq!(repo.count().block().await.unwrap(), Some(2));

    // delete_all empties the table.
    repo.delete_all().block().await.unwrap();
    assert_eq!(repo.count().block().await.unwrap(), Some(0));
}

/// Specification queries: eq / and / or / not / like, plus the streaming
/// `find_by_spec`.
pub async fn specification_queries(db: Db) {
    let repo = reactive_repo(db);
    repo.save_all(vec![
        User::new("u1", "alice", 10, true),
        User::new("u2", "bob", 20, true),
        User::new("u3", "carol", 30, false),
        User::new("u4", "dave", 40, false),
    ])
    .collect_list()
    .block()
    .await
    .unwrap();

    // active = true
    let active = Specification::pred(Predicate::new("active", Op::Eq, true));
    let rows = repo
        .find_by_spec(active.clone())
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 2, "two active users");
    assert!(rows.iter().all(|u| u.active));

    // (active = true) AND (score >= 20)
    let combo = active.clone() & Specification::pred(Predicate::new("score", Op::Gte, 20));
    let rows = repo
        .find_by_spec(combo)
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "u2");

    // (score < 15) OR (score > 35)
    let either = Specification::pred(Predicate::new("score", Op::Lt, 15))
        | Specification::pred(Predicate::new("score", Op::Gt, 35));
    let mut rows = repo
        .find_by_spec(either)
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "u1");
    assert_eq!(rows[1].id, "u4");

    // NOT (active = true)
    let inactive = !active;
    let rows = repo
        .find_by_spec(inactive)
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|u| !u.active));
}

/// Pageable specification queries: ORDER BY + LIMIT/OFFSET windows.
pub async fn pageable_queries(db: Db) {
    let repo = reactive_repo(db);
    for i in 1..=6 {
        repo.save(User::new(
            &format!("u{i}"),
            &format!("user{i}"),
            (i as i64) * 10,
            true,
        ))
        .block()
        .await
        .unwrap();
    }

    let all = Specification::all();
    let sort = RequestSort::of([Order::new("score", Direction::Asc)]);

    // page 1, size 2, score ASC -> u1(10), u2(20)
    let page1 = repo
        .find_by_spec_paged(all.clone(), Pageable::of(1, 2, sort.clone()).unwrap())
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].id, "u1");
    assert_eq!(page1[1].id, "u2");

    // page 2, size 2 -> u3(30), u4(40)
    let page2 = repo
        .find_by_spec_paged(all.clone(), Pageable::of(2, 2, sort.clone()).unwrap())
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].id, "u3");
    assert_eq!(page2[1].id, "u4");

    // score DESC, page 1, size 1 -> u6(60)
    let desc = RequestSort::of([Order::new("score", Direction::Desc)]);
    let top = repo
        .find_by_spec_paged(all, Pageable::of(1, 1, desc).unwrap())
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].id, "u6");
}

/// The blocking [`Repository`]: `find` (Filter + Page envelope), `find_page`
/// (Pageable), `find_by_id`, and the `DataError::NotFound` path.
pub async fn blocking_filter_and_page(db: Db) {
    use firefly_data::{DataError, Filter};
    let repo = blocking_repo(db);
    for i in 1..=5 {
        repo.save(User::new(
            &format!("u{i}"),
            &format!("user{i}"),
            (i as i64) * 10,
            i % 2 == 0,
        ))
        .await
        .unwrap();
    }

    // find_by_id miss -> NotFound.
    assert_eq!(
        repo.find_by_id(&"ghost".to_string()).await,
        Err(DataError::NotFound)
    );
    // hit.
    let u3 = repo.find_by_id(&"u3".to_string()).await.unwrap();
    assert_eq!(u3.name, "user3");

    // Filter: active = true, sorted, first page of 2.
    let page = repo
        .find(
            &Filter::new()
                .where_eq("active", true)
                .order_by("score", Direction::Asc)
                .paged(0, 2),
        )
        .await
        .unwrap();
    // active users are u2 (20), u4 (40) -> total 2, first page has both.
    assert_eq!(page.total_elements, 2);
    assert_eq!(page.content.len(), 2);
    assert_eq!(page.content[0].id, "u2");

    // find_page via Pageable (1-based page 1, size 3, score DESC).
    let pageable = Pageable::of(1, 3, RequestSort::of([Order::desc("score")])).unwrap();
    let page = repo.find_page(&pageable).await.unwrap();
    assert_eq!(page.total_elements, 5);
    assert_eq!(page.content.len(), 3);
    assert_eq!(page.content[0].id, "u5"); // highest score first
}

/// Auditing: an [`Auditor`] auto-stamps `created_*` on insert and moves
/// `updated_*` on update.
pub async fn auditing_stamps_on_write(db: Db) {
    let backend = db.backend();
    let repo = reactive_repo(db.clone())
        .with_auditor(Auditor::with_user_provider(fixed_user_provider("alice")));

    repo.save(User::new("u1", "alice", 1, true))
        .block()
        .await
        .unwrap();

    // Read the audit columns straight from the table to verify stamping.
    let (created_by, updated_by, created_at, updated_at) =
        read_audit_columns(&db, backend, "u1").await;
    assert_eq!(created_by.as_deref(), Some("alice"), "created_by stamped");
    assert_eq!(updated_by.as_deref(), Some("alice"), "updated_by stamped");
    assert!(created_at.is_some(), "created_at stamped");
    assert!(updated_at.is_some(), "updated_at stamped");
    let first_created = created_at.clone();

    // Update by a different user -> created_* preserved, updated_* move.
    let repo2 = reactive_repo(db.clone())
        .with_auditor(Auditor::with_user_provider(fixed_user_provider("bob")));
    repo2
        .save(User::new("u1", "alice", 2, true))
        .block()
        .await
        .unwrap();
    let (created_by, updated_by, created_at, _updated_at) =
        read_audit_columns(&db, backend, "u1").await;
    assert_eq!(
        created_by.as_deref(),
        Some("alice"),
        "created_by preserved on update"
    );
    assert_eq!(
        updated_by.as_deref(),
        Some("bob"),
        "updated_by moved on update"
    );
    assert_eq!(created_at, first_created, "created_at preserved on update");
}

/// Reads the four audit columns (as text) for a row, across backends.
async fn read_audit_columns(
    db: &Db,
    backend: firefly_data_sqlx::Backend,
    id: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    use firefly_data_sqlx::Backend;
    // created_at / updated_at are read as text via a cast so the test stays
    // dialect-light; created_by / updated_by are text already.
    let sql = match backend {
        Backend::Postgres => {
            r#"SELECT "created_by", "updated_by", "created_at"::text AS ca, "updated_at"::text AS ua FROM "users" WHERE "id" = $1"#
        }
        Backend::MySql => {
            "SELECT `created_by`, `updated_by`, CAST(`created_at` AS CHAR) AS ca, CAST(`updated_at` AS CHAR) AS ua FROM `users` WHERE `id` = ?"
        }
        Backend::Sqlite => {
            r#"SELECT "created_by", "updated_by", "created_at" AS ca, "updated_at" AS ua FROM "users" WHERE "id" = ?"#
        }
    };
    use sqlx::Row;
    match db {
        #[cfg(feature = "postgres")]
        Db::Postgres(pool) => {
            let row = sqlx::query(sql).bind(id).fetch_one(pool).await.unwrap();
            (
                row.try_get("created_by").ok(),
                row.try_get("updated_by").ok(),
                row.try_get("ca").ok(),
                row.try_get("ua").ok(),
            )
        }
        #[cfg(feature = "mysql")]
        Db::MySql(pool) => {
            let row = sqlx::query(sql).bind(id).fetch_one(pool).await.unwrap();
            (
                row.try_get("created_by").ok(),
                row.try_get("updated_by").ok(),
                row.try_get("ca").ok(),
                row.try_get("ua").ok(),
            )
        }
        #[cfg(feature = "sqlite")]
        Db::Sqlite(pool) => {
            let row = sqlx::query(sql).bind(id).fetch_one(pool).await.unwrap();
            (
                row.try_get("created_by").ok(),
                row.try_get("updated_by").ok(),
                row.try_get("ca").ok(),
                row.try_get("ua").ok(),
            )
        }
    }
}

/// Soft-delete: with a [`SoftDeletePolicy`], `delete` stamps `deleted_at`
/// rather than removing the row, and every read hides soft-deleted rows.
pub async fn soft_delete_hides_rows(db: Db) {
    let repo = reactive_repo(db.clone()).with_soft_delete(SoftDeletePolicy::new());

    repo.save_all(vec![
        User::new("u1", "alice", 10, true),
        User::new("u2", "bob", 20, true),
        User::new("u3", "carol", 30, true),
    ])
    .collect_list()
    .block()
    .await
    .unwrap();
    assert_eq!(repo.count().block().await.unwrap(), Some(3));

    // Soft-delete u2.
    repo.delete_by_id("u2".into()).block().await.unwrap();

    // Hidden from find_by_id / exists / count / find_all.
    assert_eq!(repo.find_by_id("u2".into()).block().await.unwrap(), None);
    assert!(!repo
        .exists_by_id("u2".into())
        .block()
        .await
        .unwrap()
        .unwrap());
    assert_eq!(repo.count().block().await.unwrap(), Some(2));
    let ids: Vec<String> = repo
        .find_all()
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|u| u.id)
        .collect();
    assert!(!ids.contains(&"u2".to_string()));
    assert_eq!(ids.len(), 2);

    // But the physical row still exists (the deleted_at stamp is set).
    let physical = raw_count(&db).await;
    assert_eq!(physical, 3, "soft delete keeps the physical row");

    // Specification reads also hide soft-deleted rows.
    let rows = repo
        .find_by_spec(Specification::all())
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 2);

    // delete_all soft-deletes the rest.
    repo.delete_all().block().await.unwrap();
    assert_eq!(repo.count().block().await.unwrap(), Some(0));
    assert_eq!(raw_count(&db).await, 3, "delete_all keeps physical rows");
}

/// Regression for the save-after-soft-delete bug: with a [`SoftDeletePolicy`]
/// wired, an UPSERT of a previously soft-deleted row must *resurrect* it —
/// clearing `deleted_at` — so the post-write read through the live-row guard
/// finds the persisted row and `save` / `save_all` / blocking `save` emit it,
/// rather than reporting empty / NotFound despite a successful write.
pub async fn save_after_soft_delete_resurrects(db: Db) {
    use firefly_data::DataError;
    let repo = reactive_repo(db.clone()).with_soft_delete(SoftDeletePolicy::new());

    // Persist then soft-delete u1.
    repo.save(User::new("u1", "alice", 10, true))
        .block()
        .await
        .unwrap();
    repo.delete_by_id("u1".into()).block().await.unwrap();
    // Hidden, but the physical row is still there with its deleted_at stamp.
    assert_eq!(repo.find_by_id("u1".into()).block().await.unwrap(), None);
    assert_eq!(raw_count(&db).await, 1);

    // Re-saving the soft-deleted row must return the persisted value (not an
    // empty Mono) AND make it live again.
    let resurrected = repo
        .save(User::new("u1", "alice", 99, false))
        .block()
        .await
        .unwrap();
    assert_eq!(
        resurrected,
        Some(User::new("u1", "alice", 99, false)),
        "reactive save after soft-delete must emit the persisted row"
    );
    assert_eq!(
        repo.find_by_id("u1".into()).block().await.unwrap(),
        Some(User::new("u1", "alice", 99, false)),
        "the row is live again after re-save"
    );
    assert_eq!(repo.count().block().await.unwrap(), Some(1));
    assert_eq!(raw_count(&db).await, 1, "no duplicate row was inserted");

    // save_all over a soft-deleted row likewise resurrects + streams it back.
    repo.delete_by_id("u1".into()).block().await.unwrap();
    assert_eq!(repo.find_by_id("u1".into()).block().await.unwrap(), None);
    let streamed = repo
        .save_all(vec![User::new("u1", "alice", 7, true)])
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        streamed,
        vec![User::new("u1", "alice", 7, true)],
        "save_all after soft-delete must stream the persisted row"
    );
    assert_eq!(
        repo.find_by_id("u1".into()).block().await.unwrap(),
        Some(User::new("u1", "alice", 7, true))
    );

    // The blocking Repository::save must NOT return NotFound after a re-save
    // of a soft-deleted row.
    let blocking = blocking_repo(db.clone()).with_soft_delete(SoftDeletePolicy::new());
    blocking.delete(&"u1".to_string()).await.unwrap();
    assert_eq!(
        blocking.find_by_id(&"u1".to_string()).await,
        Err(DataError::NotFound)
    );
    let saved = blocking
        .save(User::new("u1", "alice", 5, false))
        .await
        .expect("blocking save after soft-delete must not be NotFound");
    assert_eq!(saved, User::new("u1", "alice", 5, false));
    assert_eq!(
        blocking.find_by_id(&"u1".to_string()).await.unwrap(),
        User::new("u1", "alice", 5, false)
    );
}

/// Regression for the value-driven timestamp mis-typing bug: a text column
/// holding a value that *happens* to parse as an RFC 3339 instant must be
/// bound and round-tripped as plain text — not silently coerced to a
/// timestamp parameter (which would type-mismatch a TEXT/VARCHAR column on
/// Postgres/MySQL or change comparison semantics).
pub async fn rfc3339_text_value_round_trips(db: Db) {
    use firefly_data::{Filter, Op, Predicate};
    let repo = reactive_repo(db.clone());
    let blocking = blocking_repo(db);

    // The `name` column is TEXT/VARCHAR; store an ISO-8601 instant in it.
    let iso = "2026-06-13T10:30:00+00:00";
    let saved = repo
        .save(User::new("u1", iso, 1, true))
        .block()
        .await
        .unwrap();
    assert_eq!(
        saved,
        Some(User::new("u1", iso, 1, true)),
        "an RFC3339-looking text value persists + reads back verbatim"
    );

    // A WHERE filter on that text value must also bind as text and match.
    let page = blocking
        .find(&Filter::new().add(Predicate::new("name", Op::Eq, iso)))
        .await
        .expect("filtering a text column on an RFC3339-looking value must not type-mismatch");
    assert_eq!(page.total_elements, 1, "the text equality filter matches");
    assert_eq!(page.content[0].name, iso);
}

/// Raw physical row count (bypassing the soft-delete guard).
async fn raw_count(db: &Db) -> i64 {
    use sqlx::Row;
    let sql = match db.backend() {
        firefly_data_sqlx::Backend::MySql => "SELECT COUNT(*) FROM `users`",
        _ => r#"SELECT COUNT(*) FROM "users""#,
    };
    match db {
        #[cfg(feature = "postgres")]
        Db::Postgres(pool) => sqlx::query(sql)
            .fetch_one(pool)
            .await
            .unwrap()
            .try_get::<i64, _>(0)
            .unwrap(),
        #[cfg(feature = "mysql")]
        Db::MySql(pool) => sqlx::query(sql)
            .fetch_one(pool)
            .await
            .unwrap()
            .try_get::<i64, _>(0)
            .unwrap(),
        #[cfg(feature = "sqlite")]
        Db::Sqlite(pool) => sqlx::query(sql)
            .fetch_one(pool)
            .await
            .unwrap()
            .try_get::<i64, _>(0)
            .unwrap(),
    }
}
