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

//! The generic relational repositories — [`SqlxReactiveRepository`] and
//! [`SqlxRepository`] — implementing the firefly-data ports over sqlx for
//! Postgres, MySQL, **and** SQLite from one codebase.
//!
//! Both repositories are built over a [`Db`] (a backend-tagged pool), a
//! [`TableConfig`], a [`SqlxRowMapper`] (reads), and a [`RowWriter`]
//! (writes). They pick the matching [`SqlDialect`](firefly_data::SqlDialect)
//! at runtime from the pool, compile
//! [`Filter`](firefly_data::Filter) /
//! [`Specification`](firefly_data::Specification) /
//! [`Pageable`](firefly_data::Pageable) through it, and use a dialect-aware
//! `UPSERT` for `save`.
//!
//! - **[`SqlxReactiveRepository`]** is the Spring Data R2DBC analogue: every
//!   read **streams** rows lazily as a [`Flux`] off sqlx's
//!   [`fetch`](sqlx::Executor) row stream — there is no collect-then-emit
//!   buffering. It implements
//!   [`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository) and
//!   [`ReactiveSpecificationRepository`](firefly_data::ReactiveSpecificationRepository).
//! - **[`SqlxRepository`]** is the blocking-style
//!   [`Repository`](firefly_data::Repository) (one awaited value per call),
//!   sharing all the same SQL.
//!
//! Both auto-apply an optional [`Auditor`](firefly_data::Auditor) on writes
//! and an optional [`SoftDeletePolicy`](firefly_data::SoftDeletePolicy) on
//! every read **and** as a soft `delete`.

use std::sync::Arc;

use async_trait::async_trait;
use firefly_data::{
    Auditor, DataError, Filter, Page, Pageable, Repository, SoftDeletePolicy, Specification,
    SqlDialect, TableConfig,
};
use firefly_data::{ReactiveCrudRepository, ReactiveSpecificationRepository};
use firefly_kernel::FireflyError;
use firefly_reactive::{Flux, Mono};
use serde_json::Value;

use crate::binding::timestamp_value;
use crate::db::{Backend, Db};
use crate::row::{AnyRow, SqlxRowMapper};
use crate::sql;
use crate::writer::{ColumnValue, RowWriter};

/// A **reactive** relational repository over sqlx — the production
/// `ReactiveCrudRepository` for Postgres / MySQL / SQLite.
///
/// Reads stream lazily as a [`Flux`]; writes upsert with the backend's
/// `UPSERT` flavour and re-read the persisted row. An optional
/// [`Auditor`](firefly_data::Auditor) stamps audit columns on every write,
/// and an optional [`SoftDeletePolicy`](firefly_data::SoftDeletePolicy)
/// hides soft-deleted rows from every read and turns `delete` into a
/// `deleted_at` stamp instead of a physical row removal.
///
/// Clone it freely: the [`Db`] pool, mapper, and writer are all `Arc`-shared.
pub struct SqlxReactiveRepository<T, ID> {
    inner: Arc<Inner<T>>,
    _id: std::marker::PhantomData<fn() -> ID>,
}

/// Shared state behind both repository views.
struct Inner<T> {
    db: Db,
    config: TableConfig,
    mapper: Arc<dyn SqlxRowMapper<T>>,
    writer: Arc<dyn RowWriter<T>>,
    auditor: Option<Arc<Auditor>>,
    soft_delete: Option<SoftDeletePolicy>,
}

impl<T> Inner<T> {
    fn dialect(&self) -> Box<dyn SqlDialect + Send + Sync> {
        self.db.dialect()
    }

    fn backend(&self) -> Backend {
        self.db.backend()
    }

    /// Renders the `" WHERE …"` fragment a list read uses, injecting the
    /// soft-delete guard when a policy is configured. Returns `(where_sql,
    /// args)` where `where_sql` is empty when there is no restriction.
    fn read_where(&self, dialect: &dyn SqlDialect) -> (String, Vec<Value>) {
        match &self.soft_delete {
            Some(policy) => {
                let filter = policy.apply(Filter::new());
                filter.to_sql_with(dialect)
            }
            None => (String::new(), Vec::new()),
        }
    }

    /// Renders the `" WHERE …"` fragment for a specification read, AND-ing
    /// in the soft-delete guard when configured. `pageable` (when present
    /// and paged) contributes the ORDER BY / LIMIT / OFFSET tail.
    fn spec_where(
        &self,
        dialect: &dyn SqlDialect,
        spec: &Specification,
        pageable: Option<&Pageable>,
    ) -> (String, Vec<Value>, String) {
        let guarded = match &self.soft_delete {
            Some(policy) => policy.apply_spec(spec.clone()),
            None => spec.clone(),
        };
        let (frag, args) = guarded.to_sql_with(dialect);
        let where_sql = if frag.is_empty() {
            String::new()
        } else {
            format!(" WHERE {frag}")
        };
        // ORDER BY / LIMIT / OFFSET from the pageable, rendered with the
        // same dialect (no extra placeholders — LIMIT/OFFSET are literals).
        let tail = match pageable {
            Some(p) => render_pageable_tail(dialect, p),
            None => String::new(),
        };
        (where_sql, args, tail)
    }
}

/// Renders the ORDER BY / LIMIT / OFFSET tail of a [`Pageable`] for
/// `dialect`. Unpaged pageables contribute only their sort. LIMIT/OFFSET are
/// integer literals (never user input), so no placeholder is consumed.
fn render_pageable_tail(dialect: &dyn SqlDialect, pageable: &Pageable) -> String {
    let mut out = String::new();
    if pageable.sort.is_sorted() {
        let orders: Vec<String> = pageable
            .sort
            .orders
            .iter()
            .map(|o| {
                let dir = match o.direction {
                    firefly_data::Direction::Asc => "ASC",
                    firefly_data::Direction::Desc => "DESC",
                };
                format!("{} {dir}", dialect.quote_ident(&o.property))
            })
            .collect();
        out.push_str(" ORDER BY ");
        out.push_str(&orders.join(", "));
    }
    if pageable.is_paged() {
        out.push_str(&format!(
            " LIMIT {} OFFSET {}",
            pageable.size,
            pageable.offset()
        ));
    }
    out
}

impl<T, ID> Clone for SqlxReactiveRepository<T, ID> {
    fn clone(&self) -> Self {
        SqlxReactiveRepository {
            inner: Arc::clone(&self.inner),
            _id: std::marker::PhantomData,
        }
    }
}

impl<T, ID> SqlxReactiveRepository<T, ID>
where
    T: Send + Sync + 'static,
    ID: Into<Value> + Clone + Send + Sync + 'static,
{
    /// Builds a repository over `db`, a [`TableConfig`], a row `mapper`, and
    /// a `writer`. No auditing or soft-delete is applied; chain
    /// [`SqlxReactiveRepository::with_auditor`] /
    /// [`SqlxReactiveRepository::with_soft_delete`] to add them.
    pub fn new(
        db: Db,
        config: TableConfig,
        mapper: impl SqlxRowMapper<T> + 'static,
        writer: impl RowWriter<T> + 'static,
    ) -> Self {
        SqlxReactiveRepository {
            inner: Arc::new(Inner {
                db,
                config,
                mapper: Arc::new(mapper),
                writer: Arc::new(writer),
                auditor: None,
                soft_delete: None,
            }),
            _id: std::marker::PhantomData,
        }
    }

    /// Returns a copy of this repository that stamps audit columns
    /// (`created_at` / `updated_at` / `created_by` / `updated_by`) on every
    /// write via `auditor`.
    ///
    /// Audit stamping is applied by the [`RowWriter`] in practice (the
    /// writer is the only thing that knows the entity's audit fields); the
    /// repository resolves the current user from the auditor and exposes it
    /// to writes that opt in. See [`SqlxReactiveRepository::auditor`].
    pub fn with_auditor(self, auditor: Auditor) -> Self {
        let mut inner = unwrap_or_clone(self.inner);
        inner.auditor = Some(Arc::new(auditor));
        SqlxReactiveRepository {
            inner: Arc::new(inner),
            _id: std::marker::PhantomData,
        }
    }

    /// Returns a copy of this repository that hides soft-deleted rows from
    /// every read and turns [`delete_by_id`](ReactiveCrudRepository::delete_by_id)
    /// into a `deleted_at` stamp rather than a physical `DELETE`.
    pub fn with_soft_delete(self, policy: SoftDeletePolicy) -> Self {
        let mut inner = unwrap_or_clone(self.inner);
        inner.soft_delete = Some(policy);
        SqlxReactiveRepository {
            inner: Arc::new(inner),
            _id: std::marker::PhantomData,
        }
    }

    /// The configured [`Auditor`], if any. A [`RowWriter`] can read it (via
    /// a captured clone) to stamp audit columns; the repository also calls
    /// it internally so the stamps move on every write.
    pub fn auditor(&self) -> Option<&Auditor> {
        self.inner.auditor.as_deref()
    }

    /// The configured [`SoftDeletePolicy`], if any.
    pub fn soft_delete_policy(&self) -> Option<&SoftDeletePolicy> {
        self.inner.soft_delete.as_ref()
    }

    /// Streams the rows of `sql` (bound to `args`) as a lazy [`Flux<T>`],
    /// decoding each row through the [`SqlxRowMapper`] as it arrives off the
    /// backend's row stream. The streaming primitive every read is built on
    /// — and the hook for custom derived queries projecting
    /// [`TableConfig::columns`].
    pub fn stream_query(&self, sql: String, args: Vec<Value>) -> Flux<T> {
        let inner = Arc::clone(&self.inner);
        match inner.backend() {
            #[cfg(feature = "postgres")]
            Backend::Postgres => stream_pg(inner, sql, args),
            #[cfg(feature = "mysql")]
            Backend::MySql => stream_mysql(inner, sql, args),
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => stream_sqlite(inner, sql, args),
            #[allow(unreachable_patterns)]
            _ => Flux::error(no_backend_err()),
        }
    }

    /// Re-reads the row with `id` and maps it, returning a present value on
    /// a hit and an empty [`Mono`] on a miss — used by `save` to return the
    /// persisted row after an `UPSERT` (no `RETURNING`, for MySQL parity).
    fn read_by_id_mono(&self, id: ID) -> Mono<T> {
        let inner = Arc::clone(&self.inner);
        let id_value: Value = id.into();
        Mono::from_result_future(async move { fetch_one_by_id(inner, id_value).await })
            .on_error_resume(|e| {
                if e.code == EMPTY_CODE {
                    Mono::empty()
                } else {
                    Mono::error(e)
                }
            })
    }
}

/// Clones the inner state out of an `Arc` (cloning the contents when the
/// `Arc` is shared) so a builder method can mutate a fresh copy. The
/// [`Auditor`] is held behind its own `Arc` so it survives the clone without
/// requiring `Auditor: Clone` (it is not, in firefly-data).
fn unwrap_or_clone<T>(inner: Arc<Inner<T>>) -> Inner<T> {
    Inner {
        db: inner.db.clone(),
        config: inner.config.clone(),
        mapper: Arc::clone(&inner.mapper),
        writer: Arc::clone(&inner.writer),
        auditor: inner.auditor.clone(),
        soft_delete: inner.soft_delete.clone(),
    }
}

// ---------------------------------------------------------------------------
// Per-backend streaming + single-row helpers.
// ---------------------------------------------------------------------------

const EMPTY_CODE: &str = "FIREFLY_DATA_SQLX_EMPTY";

fn empty_sentinel() -> FireflyError {
    FireflyError::new(EMPTY_CODE, "Empty", 404, "no row")
}

fn no_backend_err() -> FireflyError {
    FireflyError::internal("firefly/data-sqlx: no backend feature enabled")
}

fn map_sqlx_err(e: sqlx::Error) -> FireflyError {
    FireflyError::internal(format!("firefly/data-sqlx: {e}"))
}

#[cfg(feature = "postgres")]
fn stream_pg<T: Send + 'static>(inner: Arc<Inner<T>>, sql: String, args: Vec<Value>) -> Flux<T> {
    use futures::StreamExt;
    let Db::Postgres(pool) = inner.db.clone() else {
        return Flux::error(no_backend_err());
    };
    let mapper = Arc::clone(&inner.mapper);
    Flux::from_stream(async_stream::try_stream! {
        let mut query = sqlx::query(&sql);
        for a in &args {
            query = crate::binding::bind_pg(query, a);
        }
        let mut rows = query.fetch(&pool);
        while let Some(row) = rows.next().await {
            let row = row.map_err(map_sqlx_err)?;
            let any = AnyRow::Postgres(&row);
            yield mapper.map_row(&any)?;
        }
    })
}

#[cfg(feature = "mysql")]
fn stream_mysql<T: Send + 'static>(inner: Arc<Inner<T>>, sql: String, args: Vec<Value>) -> Flux<T> {
    use futures::StreamExt;
    let Db::MySql(pool) = inner.db.clone() else {
        return Flux::error(no_backend_err());
    };
    let mapper = Arc::clone(&inner.mapper);
    Flux::from_stream(async_stream::try_stream! {
        let mut query = sqlx::query(&sql);
        for a in &args {
            query = crate::binding::bind_mysql(query, a);
        }
        let mut rows = query.fetch(&pool);
        while let Some(row) = rows.next().await {
            let row = row.map_err(map_sqlx_err)?;
            let any = AnyRow::MySql(&row);
            yield mapper.map_row(&any)?;
        }
    })
}

#[cfg(feature = "sqlite")]
fn stream_sqlite<T: Send + 'static>(
    inner: Arc<Inner<T>>,
    sql: String,
    args: Vec<Value>,
) -> Flux<T> {
    use futures::StreamExt;
    let Db::Sqlite(pool) = inner.db.clone() else {
        return Flux::error(no_backend_err());
    };
    let mapper = Arc::clone(&inner.mapper);
    Flux::from_stream(async_stream::try_stream! {
        let mut query = sqlx::query(&sql);
        for a in &args {
            query = crate::binding::bind_sqlite(query, a);
        }
        let mut rows = query.fetch(&pool);
        while let Some(row) = rows.next().await {
            let row = row.map_err(map_sqlx_err)?;
            let any = AnyRow::Sqlite(&row);
            yield mapper.map_row(&any)?;
        }
    })
}

/// Fetches the single row with `id` (honouring the soft-delete guard) and
/// maps it, or returns the [`empty_sentinel`] when no live row matches.
async fn fetch_one_by_id<T: Send + 'static>(
    inner: Arc<Inner<T>>,
    id: Value,
) -> Result<T, FireflyError> {
    let dialect = inner.dialect();
    // SELECT … WHERE id = $1 [AND deleted_at IS NULL]
    let mut sql = sql::select_by_id(&inner.config, dialect.as_ref());
    let args: Vec<Value> = vec![id];
    if let Some(policy) = &inner.soft_delete {
        // Append the live-row guard at the next placeholder index.
        let guard_col = dialect.quote_ident(policy.column());
        sql.push_str(&format!(" AND {guard_col} IS NULL"));
    }
    match inner.backend() {
        #[cfg(feature = "postgres")]
        Backend::Postgres => fetch_one_pg(inner, sql, args).await,
        #[cfg(feature = "mysql")]
        Backend::MySql => fetch_one_mysql(inner, sql, args).await,
        #[cfg(feature = "sqlite")]
        Backend::Sqlite => fetch_one_sqlite(inner, sql, args).await,
        #[allow(unreachable_patterns)]
        _ => Err(no_backend_err()),
    }
}

#[cfg(feature = "postgres")]
async fn fetch_one_pg<T: Send + 'static>(
    inner: Arc<Inner<T>>,
    sql: String,
    args: Vec<Value>,
) -> Result<T, FireflyError> {
    let Db::Postgres(pool) = &inner.db else {
        return Err(no_backend_err());
    };
    let mut query = sqlx::query(&sql);
    for a in &args {
        query = crate::binding::bind_pg(query, a);
    }
    let row = query.fetch_optional(pool).await.map_err(map_sqlx_err)?;
    match row {
        Some(r) => inner.mapper.map_row(&AnyRow::Postgres(&r)),
        None => Err(empty_sentinel()),
    }
}

#[cfg(feature = "mysql")]
async fn fetch_one_mysql<T: Send + 'static>(
    inner: Arc<Inner<T>>,
    sql: String,
    args: Vec<Value>,
) -> Result<T, FireflyError> {
    let Db::MySql(pool) = &inner.db else {
        return Err(no_backend_err());
    };
    let mut query = sqlx::query(&sql);
    for a in &args {
        query = crate::binding::bind_mysql(query, a);
    }
    let row = query.fetch_optional(pool).await.map_err(map_sqlx_err)?;
    match row {
        Some(r) => inner.mapper.map_row(&AnyRow::MySql(&r)),
        None => Err(empty_sentinel()),
    }
}

#[cfg(feature = "sqlite")]
async fn fetch_one_sqlite<T: Send + 'static>(
    inner: Arc<Inner<T>>,
    sql: String,
    args: Vec<Value>,
) -> Result<T, FireflyError> {
    let Db::Sqlite(pool) = &inner.db else {
        return Err(no_backend_err());
    };
    let mut query = sqlx::query(&sql);
    for a in &args {
        query = crate::binding::bind_sqlite(query, a);
    }
    let row = query.fetch_optional(pool).await.map_err(map_sqlx_err)?;
    match row {
        Some(r) => inner.mapper.map_row(&AnyRow::Sqlite(&r)),
        None => Err(empty_sentinel()),
    }
}

/// Runs a write statement (`args` bound as scalars), returning the affected
/// row count. Used for `UPSERT`, `DELETE`, and the soft-delete `UPDATE`.
async fn execute_write<T: Send + 'static>(
    inner: &Arc<Inner<T>>,
    sql: String,
    args: Vec<Value>,
) -> Result<u64, FireflyError> {
    match inner.backend() {
        #[cfg(feature = "postgres")]
        Backend::Postgres => {
            let Db::Postgres(pool) = &inner.db else {
                return Err(no_backend_err());
            };
            let mut query = sqlx::query(&sql);
            for a in &args {
                query = crate::binding::bind_pg(query, a);
            }
            let r = query.execute(pool).await.map_err(map_sqlx_err)?;
            Ok(r.rows_affected())
        }
        #[cfg(feature = "mysql")]
        Backend::MySql => {
            let Db::MySql(pool) = &inner.db else {
                return Err(no_backend_err());
            };
            let mut query = sqlx::query(&sql);
            for a in &args {
                query = crate::binding::bind_mysql(query, a);
            }
            let r = query.execute(pool).await.map_err(map_sqlx_err)?;
            Ok(r.rows_affected())
        }
        #[cfg(feature = "sqlite")]
        Backend::Sqlite => {
            let Db::Sqlite(pool) = &inner.db else {
                return Err(no_backend_err());
            };
            let mut query = sqlx::query(&sql);
            for a in &args {
                query = crate::binding::bind_sqlite(query, a);
            }
            let r = query.execute(pool).await.map_err(map_sqlx_err)?;
            Ok(r.rows_affected())
        }
        #[allow(unreachable_patterns)]
        _ => Err(no_backend_err()),
    }
}

/// Reads a single `i64` scalar (COUNT / EXISTS), bound to `args`.
async fn scalar_i64<T: Send + 'static>(
    inner: &Arc<Inner<T>>,
    sql: String,
    args: Vec<Value>,
) -> Result<i64, FireflyError> {
    match inner.backend() {
        #[cfg(feature = "postgres")]
        Backend::Postgres => {
            let Db::Postgres(pool) = &inner.db else {
                return Err(no_backend_err());
            };
            let mut query = sqlx::query(&sql);
            for a in &args {
                query = crate::binding::bind_pg(query, a);
            }
            let row = query.fetch_one(pool).await.map_err(map_sqlx_err)?;
            AnyRow::Postgres(&row).try_get_index_i64(0)
        }
        #[cfg(feature = "mysql")]
        Backend::MySql => {
            let Db::MySql(pool) = &inner.db else {
                return Err(no_backend_err());
            };
            let mut query = sqlx::query(&sql);
            for a in &args {
                query = crate::binding::bind_mysql(query, a);
            }
            let row = query.fetch_one(pool).await.map_err(map_sqlx_err)?;
            AnyRow::MySql(&row).try_get_index_i64(0)
        }
        #[cfg(feature = "sqlite")]
        Backend::Sqlite => {
            let Db::Sqlite(pool) = &inner.db else {
                return Err(no_backend_err());
            };
            let mut query = sqlx::query(&sql);
            for a in &args {
                query = crate::binding::bind_sqlite(query, a);
            }
            let row = query.fetch_one(pool).await.map_err(map_sqlx_err)?;
            AnyRow::Sqlite(&row).try_get_index_i64(0)
        }
        #[allow(unreachable_patterns)]
        _ => Err(no_backend_err()),
    }
}

impl AnyRow<'_> {
    /// Reads column 0 as an `i64`, used for `COUNT(*)` (`INT8` / `BIGINT`)
    /// and the `EXISTS` `CASE` (`INT4` on Postgres, integer elsewhere).
    ///
    /// Because the integer SQL type differs across these statements and
    /// backends, the decode tries `i64` first and falls back to `i32` (the
    /// width Postgres gives a bare integer literal) so a single accessor
    /// serves both `COUNT` and `EXISTS`.
    fn try_get_index_i64(&self, index: usize) -> Result<i64, FireflyError> {
        match self {
            #[cfg(feature = "postgres")]
            AnyRow::Postgres(r) => {
                use sqlx::Row;
                r.try_get::<i64, _>(index)
                    .or_else(|_| r.try_get::<i32, _>(index).map(i64::from))
                    .map_err(map_sqlx_err)
            }
            #[cfg(feature = "mysql")]
            AnyRow::MySql(r) => {
                use sqlx::Row;
                r.try_get::<i64, _>(index)
                    .or_else(|_| r.try_get::<i32, _>(index).map(i64::from))
                    .map_err(map_sqlx_err)
            }
            #[cfg(feature = "sqlite")]
            AnyRow::Sqlite(r) => {
                use sqlx::Row;
                r.try_get::<i64, _>(index)
                    .or_else(|_| r.try_get::<i32, _>(index).map(i64::from))
                    .map_err(map_sqlx_err)
            }
            AnyRow::_Phantom(_) => Err(no_backend_err()),
        }
    }
}

/// Reports whether a row with `id` already exists (ignoring the soft-delete
/// guard — an existing soft-deleted row is still an *update*, not a fresh
/// insert). Used to decide insert-vs-update for audit stamping.
async fn row_exists<T: Send + 'static>(
    inner: &Arc<Inner<T>>,
    id: &Value,
) -> Result<bool, FireflyError> {
    let dialect = inner.dialect();
    let sql = sql::exists_by_id(&inner.config, dialect.as_ref());
    let n = scalar_i64(inner, sql, vec![id.clone()]).await?;
    Ok(n != 0)
}

/// Persists `entity` with a dialect-aware `UPSERT`, auto-applying the
/// configured [`Auditor`] (insert vs update is decided by whether the row
/// already exists), and returns the entity's id [`Value`] so the caller can
/// re-read the persisted row.
async fn do_upsert<T: Send + 'static>(
    inner: &Arc<Inner<T>>,
    entity: &T,
) -> Result<Value, FireflyError> {
    let base_cols = inner.writer.columns(entity);
    let id_value = id_value_from_cols(&inner.config, &base_cols)?;
    let is_insert = match &inner.auditor {
        Some(_) => !row_exists(inner, &id_value).await?,
        None => true, // audit-irrelevant; columns() is used directly
    };
    let mut cols = inner
        .writer
        .columns_audited(entity, inner.auditor.as_deref(), is_insert);
    // With a soft-delete policy configured, an UPSERT must *resurrect* a row
    // that was previously soft-deleted: clear its `deleted_at` so the
    // post-write read (which always appends the live-row guard) finds the
    // persisted row, matching the Mongo adapter's whole-document replace. A
    // RowWriter never emits the soft-delete column itself, so inject a
    // `deleted_at = NULL` when the writer has not already set it.
    if let Some(policy) = &inner.soft_delete {
        let del_col = policy.column();
        if !cols.iter().any(|c| c.column == del_col) {
            // A *typed* NULL timestamp (not a text NULL) so Postgres accepts
            // it against a TIMESTAMPTZ column in the INSERT VALUES list.
            cols.push(ColumnValue {
                column: del_col.to_string(),
                value: crate::binding::timestamp_null_value(),
            });
        }
    }
    let dialect = inner.dialect();
    let (sql, args) = sql::upsert_sql(&inner.config, dialect.as_ref(), inner.backend(), &cols);
    execute_write(inner, sql, args).await?;
    Ok(id_value)
}

// ---------------------------------------------------------------------------
// ReactiveCrudRepository
// ---------------------------------------------------------------------------

#[async_trait]
impl<T, ID> ReactiveCrudRepository<T, ID> for SqlxReactiveRepository<T, ID>
where
    T: Send + Sync + 'static,
    ID: Into<Value> + Clone + Send + Sync + 'static,
{
    fn find_all(&self) -> Flux<T> {
        let dialect = self.inner.dialect();
        let (where_sql, args) = self.inner.read_where(dialect.as_ref());
        let sql = format!(
            "{}{}",
            sql::select_all(&self.inner.config, dialect.as_ref()),
            where_sql
        );
        self.stream_query(sql, args)
    }

    fn find_all_by_id(&self, ids: Vec<ID>) -> Flux<T> {
        if ids.is_empty() {
            return Flux::empty();
        }
        let dialect = self.inner.dialect();
        // Build an IN predicate over the id column via the Filter DSL so the
        // dialect handles array-vs-expanded binding for us.
        let id_values: Vec<Value> = ids.into_iter().map(Into::into).collect();
        let mut filter = Filter::new().add(firefly_data::Predicate::new(
            self.inner.config.id_column.clone(),
            firefly_data::Op::In,
            Value::Array(id_values),
        ));
        if let Some(policy) = &self.inner.soft_delete {
            filter = policy.apply(filter);
        }
        let (where_sql, args) = filter.to_sql_with(dialect.as_ref());
        let sql = format!(
            "{}{}",
            sql::select_all(&self.inner.config, dialect.as_ref()),
            where_sql
        );
        self.stream_query(sql, args)
    }

    fn find_by_id(&self, id: ID) -> Mono<T> {
        self.read_by_id_mono(id)
    }

    fn exists_by_id(&self, id: ID) -> Mono<bool> {
        let inner = Arc::clone(&self.inner);
        let id_value: Value = id.into();
        Mono::from_result_future(async move {
            let dialect = inner.dialect();
            let mut sql = sql::exists_by_id(&inner.config, dialect.as_ref());
            if let Some(policy) = &inner.soft_delete {
                // exists must also respect the soft-delete guard. The
                // CASE-WHEN wrapper keeps the result an integer on every
                // backend (Postgres's bare EXISTS yields a BOOL).
                let table_q = dialect.quote_ident(&inner.config.table);
                let id_q = dialect.quote_ident(&inner.config.id_column);
                let del_q = dialect.quote_ident(policy.column());
                let ph = dialect.placeholder(1);
                sql = format!(
                    "SELECT CASE WHEN EXISTS(SELECT 1 FROM {table_q} WHERE {id_q} = {ph} AND {del_q} IS NULL) THEN 1 ELSE 0 END"
                );
            }
            let n = scalar_i64(&inner, sql, vec![id_value]).await?;
            Ok(n != 0)
        })
    }

    fn save(&self, entity: T) -> Mono<T> {
        let inner = Arc::clone(&self.inner);
        let repo = self.clone();
        Mono::from_result_future(async move { do_upsert(&inner, &entity).await })
            .flat_map(move |id_value| repo.read_by_id_mono_value(id_value))
    }

    fn save_all(&self, entities: Vec<T>) -> Flux<T> {
        let inner = Arc::clone(&self.inner);
        let repo = self.clone();
        Flux::from_stream(async_stream::try_stream! {
            for entity in entities {
                let id_value = do_upsert(&inner, &entity).await?;
                let mono = repo.read_by_id_mono_value(id_value);
                if let Some(v) = mono.into_future().await? {
                    yield v;
                }
            }
        })
    }

    fn delete_by_id(&self, id: ID) -> Mono<()> {
        let inner = Arc::clone(&self.inner);
        let id_value: Value = id.into();
        Mono::from_result_future(async move {
            let dialect = inner.dialect();
            let (sql, args) = match &inner.soft_delete {
                Some(policy) => {
                    // Soft delete: stamp deleted_at = now WHERE id = $1.
                    let now = chrono::Utc::now();
                    let table_q = dialect.quote_ident(&inner.config.table);
                    let del_q = dialect.quote_ident(policy.column());
                    let id_q = dialect.quote_ident(&inner.config.id_column);
                    let set_ph = dialect.placeholder(1);
                    let id_ph = dialect.placeholder(2);
                    let sql =
                        format!("UPDATE {table_q} SET {del_q} = {set_ph} WHERE {id_q} = {id_ph}");
                    (sql, vec![timestamp_value(now), id_value])
                }
                None => {
                    let sql = sql::delete_by_id(&inner.config, dialect.as_ref());
                    (sql, vec![id_value])
                }
            };
            execute_write(&inner, sql, args).await?;
            Ok(())
        })
    }

    fn delete_all(&self) -> Mono<()> {
        let inner = Arc::clone(&self.inner);
        Mono::from_result_future(async move {
            let dialect = inner.dialect();
            let (sql, args) = match &inner.soft_delete {
                Some(policy) => {
                    let now = chrono::Utc::now();
                    let table_q = dialect.quote_ident(&inner.config.table);
                    let del_q = dialect.quote_ident(policy.column());
                    let set_ph = dialect.placeholder(1);
                    // Only stamp rows that are still live, so already-deleted
                    // rows keep their original timestamp.
                    let sql =
                        format!("UPDATE {table_q} SET {del_q} = {set_ph} WHERE {del_q} IS NULL");
                    (sql, vec![timestamp_value(now)])
                }
                None => (sql::delete_all(&inner.config, dialect.as_ref()), Vec::new()),
            };
            execute_write(&inner, sql, args).await?;
            Ok(())
        })
    }

    fn count(&self) -> Mono<u64> {
        let inner = Arc::clone(&self.inner);
        Mono::from_result_future(async move {
            let dialect = inner.dialect();
            let (where_sql, args) = inner.read_where(dialect.as_ref());
            let sql = sql::count_where(&inner.config, dialect.as_ref(), &where_sql);
            let n = scalar_i64(&inner, sql, args).await?;
            Ok(n as u64)
        })
    }
}

impl<T, ID> SqlxReactiveRepository<T, ID>
where
    T: Send + Sync + 'static,
    ID: Into<Value> + Clone + Send + Sync + 'static,
{
    /// `read_by_id_mono` keyed off an already-lowered id [`Value`] — used
    /// internally by `save` / `save_all` to re-read after an upsert.
    fn read_by_id_mono_value(&self, id: Value) -> Mono<T> {
        let inner = Arc::clone(&self.inner);
        Mono::from_result_future(async move { fetch_one_by_id(inner, id).await }).on_error_resume(
            |e| {
                if e.code == EMPTY_CODE {
                    Mono::empty()
                } else {
                    Mono::error(e)
                }
            },
        )
    }
}

/// Pulls the id column's value out of an entity's `(column, value)` pairs.
fn id_value_from_cols(config: &TableConfig, cols: &[ColumnValue]) -> Result<Value, FireflyError> {
    cols.iter()
        .find(|c| c.column == config.id_column)
        .map(|c| c.value.clone())
        .ok_or_else(|| {
            FireflyError::internal(format!(
                "firefly/data-sqlx: RowWriter did not emit id column '{}'",
                config.id_column
            ))
        })
}

// ---------------------------------------------------------------------------
// ReactiveSpecificationRepository
// ---------------------------------------------------------------------------

#[async_trait]
impl<T, ID> ReactiveSpecificationRepository<T> for SqlxReactiveRepository<T, ID>
where
    T: Send + Sync + 'static,
    ID: Into<Value> + Clone + Send + Sync + 'static,
{
    fn find_by_spec(&self, spec: Specification) -> Flux<T> {
        let dialect = self.inner.dialect();
        let (where_sql, args, _tail) = self.inner.spec_where(dialect.as_ref(), &spec, None);
        let sql = format!(
            "{}{}",
            sql::select_all(&self.inner.config, dialect.as_ref()),
            where_sql
        );
        self.stream_query(sql, args)
    }

    fn find_by_spec_paged(&self, spec: Specification, pageable: Pageable) -> Flux<T> {
        let dialect = self.inner.dialect();
        let (where_sql, args, tail) =
            self.inner
                .spec_where(dialect.as_ref(), &spec, Some(&pageable));
        let sql = format!(
            "{}{}{}",
            sql::select_all(&self.inner.config, dialect.as_ref()),
            where_sql,
            tail
        );
        self.stream_query(sql, args)
    }
}

// ---------------------------------------------------------------------------
// Blocking-style Repository over the same SQL.
// ---------------------------------------------------------------------------

/// A blocking-style relational [`Repository`] over sqlx — the awaited-value
/// twin of [`SqlxReactiveRepository`], sharing all the same dialect-aware
/// SQL, auditing, and soft-delete behaviour.
///
/// `find` honours the [`Filter`] predicates / sort / paging (the soft-delete
/// guard is injected first when a policy is configured) and returns a
/// [`Page<T>`] envelope; `find_by_id` returns [`DataError::NotFound`] on a
/// miss.
pub struct SqlxRepository<T, K> {
    inner: Arc<Inner<T>>,
    _k: std::marker::PhantomData<fn() -> K>,
}

impl<T, K> Clone for SqlxRepository<T, K> {
    fn clone(&self) -> Self {
        SqlxRepository {
            inner: Arc::clone(&self.inner),
            _k: std::marker::PhantomData,
        }
    }
}

impl<T, K> SqlxRepository<T, K>
where
    T: Send + Sync + 'static,
    K: Into<Value> + Clone + Send + Sync + 'static,
{
    /// Builds a blocking repository over `db`, a [`TableConfig`], a row
    /// `mapper`, and a `writer`.
    pub fn new(
        db: Db,
        config: TableConfig,
        mapper: impl SqlxRowMapper<T> + 'static,
        writer: impl RowWriter<T> + 'static,
    ) -> Self {
        SqlxRepository {
            inner: Arc::new(Inner {
                db,
                config,
                mapper: Arc::new(mapper),
                writer: Arc::new(writer),
                auditor: None,
                soft_delete: None,
            }),
            _k: std::marker::PhantomData,
        }
    }

    /// Returns a copy that stamps audit columns on every write.
    pub fn with_auditor(self, auditor: Auditor) -> Self {
        let mut inner = unwrap_or_clone(self.inner);
        inner.auditor = Some(Arc::new(auditor));
        SqlxRepository {
            inner: Arc::new(inner),
            _k: std::marker::PhantomData,
        }
    }

    /// Returns a copy that hides soft-deleted rows from reads and soft-deletes
    /// on `delete`.
    pub fn with_soft_delete(self, policy: SoftDeletePolicy) -> Self {
        let mut inner = unwrap_or_clone(self.inner);
        inner.soft_delete = Some(policy);
        SqlxRepository {
            inner: Arc::new(inner),
            _k: std::marker::PhantomData,
        }
    }

    /// Collects every row of `sql` (bound to `args`) into a `Vec<T>`.
    async fn fetch_all(&self, sql: String, args: Vec<Value>) -> Result<Vec<T>, FireflyError> {
        let inner = Arc::clone(&self.inner);
        let flux = match inner.backend() {
            #[cfg(feature = "postgres")]
            Backend::Postgres => stream_pg(inner, sql, args),
            #[cfg(feature = "mysql")]
            Backend::MySql => stream_mysql(inner, sql, args),
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => stream_sqlite(inner, sql, args),
            #[allow(unreachable_patterns)]
            _ => Flux::error(no_backend_err()),
        };
        Ok(flux.collect_list().into_future().await?.unwrap_or_default())
    }
}

#[async_trait]
impl<T, K> Repository<T, K> for SqlxRepository<T, K>
where
    T: Send + Sync + 'static,
    K: Into<Value> + Clone + Send + Sync + 'static,
{
    async fn find_by_id(&self, id: &K) -> Result<T, DataError> {
        let inner = Arc::clone(&self.inner);
        let id_value: Value = id.clone().into();
        match fetch_one_by_id(inner, id_value).await {
            Ok(v) => Ok(v),
            Err(e) if e.code == EMPTY_CODE => Err(DataError::NotFound),
            Err(e) => Err(DataError::Backend(e.to_string())),
        }
    }

    async fn find(&self, filter: &Filter) -> Result<Page<T>, DataError> {
        let dialect = self.inner.dialect();
        // Inject the soft-delete guard if configured.
        let effective = match &self.inner.soft_delete {
            Some(policy) => policy.apply(filter.clone()),
            None => filter.clone(),
        };
        let (where_and_tail, args) = effective.to_sql_with(dialect.as_ref());
        let select = format!(
            "{}{}",
            sql::select_all(&self.inner.config, dialect.as_ref()),
            where_and_tail
        );
        // Count uses the same predicates but no ORDER BY / LIMIT / OFFSET.
        let mut count_filter = effective.clone();
        count_filter.sorts.clear();
        count_filter.size = 0;
        let (count_where, count_args) = count_filter.to_sql_with(dialect.as_ref());
        let count_sql = sql::count_where(&self.inner.config, dialect.as_ref(), &count_where);

        let rows = self
            .fetch_all(select, args)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))?;
        let total = scalar_i64(&self.inner, count_sql, count_args)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))? as u64;
        Ok(Page::new(rows, filter.page, filter.size, total))
    }

    async fn save(&self, entity: T) -> Result<T, DataError> {
        let inner = Arc::clone(&self.inner);
        let id_value = do_upsert(&inner, &entity)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))?;
        match fetch_one_by_id(inner, id_value).await {
            Ok(v) => Ok(v),
            Err(e) if e.code == EMPTY_CODE => Err(DataError::NotFound),
            Err(e) => Err(DataError::Backend(e.to_string())),
        }
    }

    async fn delete(&self, id: &K) -> Result<(), DataError> {
        let inner = Arc::clone(&self.inner);
        let id_value: Value = id.clone().into();
        let dialect = inner.dialect();
        let (sql, args) = match &inner.soft_delete {
            Some(policy) => {
                let now = chrono::Utc::now();
                let table_q = dialect.quote_ident(&inner.config.table);
                let del_q = dialect.quote_ident(policy.column());
                let id_q = dialect.quote_ident(&inner.config.id_column);
                let set_ph = dialect.placeholder(1);
                let id_ph = dialect.placeholder(2);
                let sql = format!("UPDATE {table_q} SET {del_q} = {set_ph} WHERE {id_q} = {id_ph}");
                (sql, vec![timestamp_value(now), id_value])
            }
            None => (
                sql::delete_by_id(&inner.config, dialect.as_ref()),
                vec![id_value],
            ),
        };
        execute_write(&inner, sql, args)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))?;
        Ok(())
    }
}
