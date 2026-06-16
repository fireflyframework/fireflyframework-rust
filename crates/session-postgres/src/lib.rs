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

//! firefly-session-postgres — a Postgres-backed
//! [`firefly_session::SessionRegistry`].
//!
//! [`PostgresSessionRegistry`] is the Rust port of pyfly's
//! `PostgresSessionRegistry` (`pyfly.session.adapters.postgres_registry`). It
//! is a **durable, distributed** per-principal index of live sessions for
//! relational-only deployments (no Redis required): every application instance
//! reads and writes the same table, so the per-principal concurrency cap
//! enforced by [`firefly_session::SessionConcurrencyController`] holds across
//! the whole cluster — not just within one process (the limit of the in-process
//! [`firefly_session::MemorySessionRegistry`]).
//!
//! # Data model
//!
//! A single table indexes every principal's live sessions, keyed by the session
//! id (so the same session id can never be double-registered):
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS firefly_session_registry (
//!     session_id  TEXT PRIMARY KEY,
//!     principal   TEXT NOT NULL,
//!     created_at  BIGINT NOT NULL
//! )
//! ```
//!
//! `created_at` is the session's epoch-millis creation time, stored as a
//! `BIGINT` so it round-trips the [`SessionRegistry`] contract's `i64` exactly
//! (pyfly uses `DOUBLE PRECISION` for a float timestamp; the Rust trait's
//! timestamp is an integer, so `BIGINT` is the faithful column type). A
//! supporting index on `principal` keeps the per-principal queries fast.
//!
//! | [`SessionRegistry`] method | SQL                                                                 |
//! |----------------------------|---------------------------------------------------------------------|
//! | `register`                 | `INSERT … ON CONFLICT (session_id) DO UPDATE SET principal, created_at` |
//! | `deregister`               | `DELETE … WHERE principal = $1 AND session_id = $2`                 |
//! | `list_sessions`            | `SELECT session_id, created_at … WHERE principal = $1 ORDER BY created_at ASC` |
//! | `count`                    | `SELECT COUNT(*) … WHERE principal = $1`                            |
//!
//! The `ORDER BY created_at ASC` makes [`SessionRegistry::list_sessions`]
//! **oldest-first** (matching the in-process registry and the Redis adapter),
//! and the `ON CONFLICT … DO UPDATE` makes `register` an idempotent upsert.
//!
//! # Auto-DDL
//!
//! Like pyfly, the backing table is created **lazily and idempotently** on
//! first use: the first registry method to run executes the
//! `CREATE TABLE IF NOT EXISTS` (plus the supporting index) exactly once,
//! guarded by an async mutex so concurrent first calls don't race the DDL.
//! [`PostgresSessionRegistry::init`] forces the DDL eagerly if a caller prefers
//! to fail fast at startup rather than on the first login.
//!
//! # Custom table names
//!
//! By default the table is [`TABLE`] (`firefly_session_registry`). To target a
//! different table — e.g. to isolate parallel integration tests — construct the
//! registry with a `_with_table` constructor. The table name is validated
//! strictly (ASCII `[a-z0-9_]`, starting with a letter or underscore, at most 63
//! bytes — Postgres's identifier limit) and an invalid name is rejected rather
//! than interpolated into SQL, so there is no injection surface.
//!
//! # Lifecycle
//!
//! Unlike pyfly — whose registry is handed an already-built SQLAlchemy
//! `AsyncEngine` (lazily, via a factory) — this adapter is constructed from a
//! [`tokio_postgres::Client`] (the DI entry point) or a connection string,
//! matching `firefly-cache-postgres`. There is no `stop`: the client's lifecycle
//! belongs to its owner.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use firefly_session::{SessionRegistry, SessionConcurrencyController, ConcurrencyPolicy, Strategy};
//! use firefly_session_postgres::PostgresSessionRegistry;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let registry = Arc::new(
//!     PostgresSessionRegistry::connect("postgresql://localhost/app").await?,
//! );
//! registry.init().await?; // optional: create the table eagerly
//! let controller = SessionConcurrencyController::new(
//!     registry.clone(),
//!     ConcurrencyPolicy { max_sessions: 2, strategy: Strategy::EvictOldest },
//! );
//! controller.on_login("alice", "session-1", 1_700_000_000_000).await;
//! assert_eq!(registry.count("alice").await, 1);
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use firefly_session::SessionRegistry;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

/// Framework version stamp.
pub const VERSION: &str = "26.6.21";

/// The session-registry table name — pyfly's `pyfly_session_registry` under the
/// Rust framework's `firefly_` prefix.
pub const TABLE: &str = "firefly_session_registry";

/// The create-table-if-not-exists DDL run lazily on first use (and by
/// [`PostgresSessionRegistry::init`]) for the default [`TABLE`]. The
/// session_id-PK / principal / created_at shape mirrors pyfly's adapter, with
/// `created_at` as `BIGINT` to round-trip the [`SessionRegistry`] contract's
/// `i64` epoch-millis exactly.
pub const DDL: &str = "CREATE TABLE IF NOT EXISTS firefly_session_registry (\n    \
     session_id  TEXT PRIMARY KEY,\n    \
     principal   TEXT NOT NULL,\n    \
     created_at  BIGINT NOT NULL\n)";

/// The supporting index on `principal`, created alongside the table — pyfly's
/// `<table>_principal_idx`.
pub const INDEX_DDL: &str = "CREATE INDEX IF NOT EXISTS firefly_session_registry_principal_idx \
     ON firefly_session_registry (principal)";

/// `INSERT … ON CONFLICT (session_id) DO UPDATE` upsert — pyfly's `register`.
pub const UPSERT_SQL: &str =
    "INSERT INTO firefly_session_registry (session_id, principal, created_at) \
     VALUES ($1, $2, $3) \
     ON CONFLICT (session_id) DO UPDATE \
     SET principal = EXCLUDED.principal, created_at = EXCLUDED.created_at";

/// Single-session delete scoped to the principal — pyfly's `deregister`.
pub const DELETE_SQL: &str =
    "DELETE FROM firefly_session_registry WHERE principal = $1 AND session_id = $2";

/// Oldest-first per-principal listing — pyfly's `list_sessions`.
pub const LIST_SQL: &str = "SELECT session_id, created_at FROM firefly_session_registry \
     WHERE principal = $1 ORDER BY created_at ASC, session_id ASC";

/// Per-principal live-session count — pyfly's `count`.
pub const COUNT_SQL: &str = "SELECT COUNT(*) FROM firefly_session_registry WHERE principal = $1";

/// A durable, distributed [`firefly_session::SessionRegistry`] backed by a
/// single Postgres table, shared by every application instance.
///
/// See the [crate docs](crate) for the data model and SQL mapping. The registry
/// holds an owned [`tokio_postgres::Client`] (`tokio-postgres`'s client is
/// `Send + Sync` and pipelines concurrent queries internally) plus a one-shot
/// "table ensured" latch guarded by an async [`Mutex`] for the lazy auto-DDL.
pub struct PostgresSessionRegistry {
    client: Client,
    sql: TableSql,
    /// Lazy create-table latch: `false` until the DDL has run once. Guarded by
    /// `ensure_lock` so two concurrent first calls don't both issue the DDL.
    ensured: Mutex<bool>,
}

/// The set of statements a registry runs, rendered once from a single validated
/// table name. Building these at construction (rather than `format!`-ing per
/// call) keeps the hot path allocation-free and guarantees every statement
/// targets the same already-validated identifier.
struct TableSql {
    table: String,
    ddl: String,
    index_ddl: String,
    upsert: String,
    delete: String,
    list: String,
    count: String,
}

impl TableSql {
    /// Renders the full statement set for `table`, which **must** already have
    /// passed [`validate_table_name`].
    fn new(table: &str) -> Self {
        Self {
            table: table.to_owned(),
            ddl: format!(
                "CREATE TABLE IF NOT EXISTS {table} (\n    \
                 session_id  TEXT PRIMARY KEY,\n    \
                 principal   TEXT NOT NULL,\n    \
                 created_at  BIGINT NOT NULL\n)"
            ),
            index_ddl: format!(
                "CREATE INDEX IF NOT EXISTS {table}_principal_idx ON {table} (principal)"
            ),
            upsert: format!(
                "INSERT INTO {table} (session_id, principal, created_at) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (session_id) DO UPDATE \
                 SET principal = EXCLUDED.principal, created_at = EXCLUDED.created_at"
            ),
            delete: format!("DELETE FROM {table} WHERE principal = $1 AND session_id = $2"),
            list: format!(
                "SELECT session_id, created_at FROM {table} \
                 WHERE principal = $1 ORDER BY created_at ASC, session_id ASC"
            ),
            count: format!("SELECT COUNT(*) FROM {table} WHERE principal = $1"),
        }
    }
}

impl std::fmt::Debug for PostgresSessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresSessionRegistry")
            .field("table", &self.sql.table)
            .finish_non_exhaustive()
    }
}

impl PostgresSessionRegistry {
    /// Connects to Postgres using `conn` (a `postgresql://` URL or a
    /// `tokio-postgres` keyword/value string), spawns the connection driver
    /// task, and returns a ready registry targeting the default [`TABLE`]. The
    /// table is created lazily on first use; call
    /// [`init`](PostgresSessionRegistry::init) to create it eagerly.
    ///
    /// SQLAlchemy dialect markers (`postgresql+asyncpg://`,
    /// `postgresql+psycopg://`, `postgres+asyncpg://`) are stripped so a
    /// pyfly-style URL connects unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Backend`] if the connection string is malformed
    /// or the initial connection cannot be established.
    pub async fn connect(conn: &str) -> Result<Self, RegistryError> {
        Self::connect_with_table(conn, TABLE).await
    }

    /// Like [`connect`](PostgresSessionRegistry::connect) but targets a custom
    /// `table`.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Backend`] if `table` is not a valid, safe
    /// identifier (ASCII `[a-z0-9_]`, starting with a letter or underscore, at
    /// most 63 bytes), or if the connection string is malformed / the
    /// connection cannot be established.
    pub async fn connect_with_table(conn: &str, table: &str) -> Result<Self, RegistryError> {
        let sql = build_table_sql(table)?;
        let dsn = normalise_dsn(conn);
        let (client, connection) = tokio_postgres::connect(&dsn, NoTls)
            .await
            .map_err(backend_err)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self::from_sql(client, sql))
    }

    /// Wraps an already-established [`tokio_postgres::Client`] targeting the
    /// default [`TABLE`] — the dependency-injection entry point, paralleling
    /// pyfly's `PostgresSessionRegistry(engine_factory)`. The table is created
    /// lazily on first use.
    #[must_use]
    pub fn from_client(client: Client) -> Self {
        // The default TABLE is a compile-time-valid identifier, so building its
        // SQL cannot fail.
        Self::from_sql(client, TableSql::new(TABLE))
    }

    /// Like [`from_client`](PostgresSessionRegistry::from_client) but targets a
    /// custom `table`.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Backend`] if `table` is not a valid, safe
    /// identifier (see
    /// [`connect_with_table`](PostgresSessionRegistry::connect_with_table)).
    pub fn from_client_with_table(client: Client, table: &str) -> Result<Self, RegistryError> {
        Ok(Self::from_sql(client, build_table_sql(table)?))
    }

    /// Shared constructor: wraps `client` with a rendered statement set and an
    /// un-ensured table latch.
    fn from_sql(client: Client, sql: TableSql) -> Self {
        Self {
            client,
            sql,
            ensured: Mutex::new(false),
        }
    }

    /// The table this registry targets (the default [`TABLE`] unless built with
    /// a `_with_table` constructor).
    #[must_use]
    pub fn table(&self) -> &str {
        &self.sql.table
    }

    /// Eagerly runs the create-table-if-not-exists DDL (table + supporting
    /// index). Idempotent: safe to call repeatedly and on a table that already
    /// exists, and it flips the lazy-DDL latch so the trait methods skip the
    /// per-call ensure. Call this at startup to fail fast on a DDL/permission
    /// problem rather than on the first login.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Backend`] on a transport / DDL failure.
    pub async fn init(&self) -> Result<(), RegistryError> {
        let mut ensured = self.ensured.lock().await;
        self.run_ddl().await?;
        *ensured = true;
        Ok(())
    }

    /// Runs the table + index DDL (no latch handling).
    async fn run_ddl(&self) -> Result<(), RegistryError> {
        self.client
            .batch_execute(&self.sql.ddl)
            .await
            .map_err(backend_err)?;
        self.client
            .batch_execute(&self.sql.index_ddl)
            .await
            .map_err(backend_err)
    }

    /// Lazily ensures the table exists, exactly once, behind the async latch —
    /// pyfly's `_ensure_table`. Concurrent first callers serialize on the
    /// mutex; only the first runs the DDL.
    async fn ensure_table(&self) -> Result<(), RegistryError> {
        // Fast path: already ensured (a non-async read still needs the lock
        // because `bool` lives inside the Mutex, but it is uncontended and
        // cheap after the first call).
        let mut ensured = self.ensured.lock().await;
        if *ensured {
            return Ok(());
        }
        self.run_ddl().await?;
        *ensured = true;
        Ok(())
    }

    /// Reports whether Postgres answers a trivial `SELECT 1` — the fail-soft
    /// health probe. A failure is reported as `false` rather than an error so
    /// callers can degrade gracefully.
    pub async fn is_available(&self) -> bool {
        self.client.query_one("SELECT 1", &[]).await.is_ok()
    }
}

#[async_trait]
impl SessionRegistry for PostgresSessionRegistry {
    /// Lazily ensures the table, then `INSERT … ON CONFLICT (session_id) DO
    /// UPDATE` — pyfly's `register`. The [`SessionRegistry`] trait is
    /// infallible by contract, so a backend failure is logged and swallowed
    /// (the concurrency cap simply isn't enforced for this login rather than
    /// the login failing).
    async fn register(&self, principal: &str, session_id: &str, created_at: i64) {
        if let Err(e) = self.ensure_table().await {
            tracing::warn!(principal, session_id, error = %e, "session-postgres: ensure-table failed; skipping register");
            return;
        }
        if let Err(e) = self
            .client
            .execute(&self.sql.upsert, &[&session_id, &principal, &created_at])
            .await
        {
            tracing::warn!(principal, session_id, error = %e, "session-postgres: register upsert failed; concurrency cap not enforced for this login");
        }
    }

    /// `DELETE … WHERE principal = $1 AND session_id = $2` — pyfly's
    /// `deregister`. Idempotent (deleting a missing row affects zero rows) and
    /// infallible by contract; a backend failure is logged and swallowed.
    async fn deregister(&self, principal: &str, session_id: &str) {
        if let Err(e) = self.ensure_table().await {
            tracing::warn!(principal, session_id, error = %e, "session-postgres: ensure-table failed; skipping deregister");
            return;
        }
        if let Err(e) = self
            .client
            .execute(&self.sql.delete, &[&principal, &session_id])
            .await
        {
            tracing::warn!(principal, session_id, error = %e, "session-postgres: deregister failed");
        }
    }

    /// `SELECT session_id, created_at … WHERE principal = $1 ORDER BY
    /// created_at ASC` — pyfly's `list_sessions`. The `ORDER BY` makes the
    /// result **oldest-first**. A backend failure is logged and yields an empty
    /// list (the trait is infallible).
    async fn list_sessions(&self, principal: &str) -> Vec<(String, i64)> {
        if let Err(e) = self.ensure_table().await {
            tracing::warn!(principal, error = %e, "session-postgres: ensure-table failed; returning empty session list");
            return Vec::new();
        }
        match self.client.query(&self.sql.list, &[&principal]).await {
            Ok(rows) => rows
                .iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1)))
                .collect(),
            Err(e) => {
                tracing::warn!(principal, error = %e, "session-postgres: list_sessions query failed");
                Vec::new()
            }
        }
    }

    /// `SELECT COUNT(*) … WHERE principal = $1` — pyfly's `count`. A backend
    /// failure is logged and yields `0`.
    async fn count(&self, principal: &str) -> usize {
        if let Err(e) = self.ensure_table().await {
            tracing::warn!(principal, error = %e, "session-postgres: ensure-table failed; returning count 0");
            return 0;
        }
        match self.client.query_one(&self.sql.count, &[&principal]).await {
            Ok(row) => {
                let n: i64 = row.get(0);
                n.max(0) as usize
            }
            Err(e) => {
                tracing::warn!(principal, error = %e, "session-postgres: count query failed");
                0
            }
        }
    }
}

/// The error type surfaced by [`PostgresSessionRegistry`]'s **constructors** and
/// [`init`](PostgresSessionRegistry::init) (connection setup / DDL). The
/// [`SessionRegistry`] trait methods themselves are infallible by contract, so a
/// per-operation failure there is logged and swallowed rather than returned
/// (see each method's docs); this type reports connection / table-name / DDL
/// failures.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// A Postgres transport / protocol / DDL error, or an invalid table name.
    #[error("firefly/session-postgres backend error: {0}")]
    Backend(String),
}

/// Strips a SQLAlchemy dialect marker (`postgresql+asyncpg://`,
/// `postgresql+psycopg://`, `postgres+asyncpg://`) so a pyfly-style URL connects
/// through plain `tokio-postgres`. Connection strings without a marker pass
/// through unchanged — mirrors `firefly-cache-postgres`'s `normalise_dsn`.
#[must_use]
pub fn normalise_dsn(dsn: &str) -> String {
    for marker in [
        "postgresql+asyncpg://",
        "postgresql+psycopg://",
        "postgres+asyncpg://",
    ] {
        if let Some(rest) = dsn.strip_prefix(marker) {
            return format!("postgresql://{rest}");
        }
    }
    dsn.to_string()
}

/// Validates a registry table name as a safe, plain SQL identifier so it can be
/// rendered into statements without an injection risk: ASCII lowercase letters,
/// digits and underscores only, a leading letter or underscore (not a digit),
/// and at most 63 bytes (Postgres's identifier length limit). Returns the name
/// on success, or [`RegistryError::Backend`] describing the violation.
///
/// ```
/// use firefly_session_postgres::validate_table_name;
/// assert!(validate_table_name("firefly_session_registry").is_ok());
/// assert!(validate_table_name("fftest_sess_123").is_ok());
/// assert!(validate_table_name("x; DROP TABLE y").is_err());
/// assert!(validate_table_name("").is_err());
/// assert!(validate_table_name("9leading").is_err());
/// assert!(validate_table_name("Mixed").is_err());
/// ```
///
/// # Errors
///
/// Returns [`RegistryError::Backend`] when the name is empty, too long, starts
/// with a digit, or contains any character outside `[a-z0-9_]`.
pub fn validate_table_name(table: &str) -> Result<&str, RegistryError> {
    fn invalid(table: &str, why: &str) -> RegistryError {
        RegistryError::Backend(format!(
            "firefly/session-postgres: invalid table name {table:?}: {why}"
        ))
    }

    if table.is_empty() {
        return Err(invalid(table, "must not be empty"));
    }
    if table.len() > 63 {
        return Err(invalid(
            table,
            "must be at most 63 bytes (Postgres identifier limit)",
        ));
    }
    let mut chars = table.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_lowercase() || first == '_') {
        return Err(invalid(
            table,
            "must start with an ASCII lowercase letter or underscore",
        ));
    }
    for ch in std::iter::once(first).chain(chars) {
        if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
            return Err(invalid(
                table,
                "may contain only ASCII lowercase letters, digits and underscores",
            ));
        }
    }
    Ok(table)
}

/// Validates `table` and renders its statement set.
fn build_table_sql(table: &str) -> Result<TableSql, RegistryError> {
    validate_table_name(table).map(TableSql::new)
}

/// Wraps a [`tokio_postgres::Error`] as [`RegistryError::Backend`].
fn backend_err(e: tokio_postgres::Error) -> RegistryError {
    RegistryError::Backend(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddl_has_the_expected_table_shape() {
        assert!(DDL.contains("CREATE TABLE IF NOT EXISTS firefly_session_registry"));
        assert!(DDL.contains("session_id  TEXT PRIMARY KEY"));
        assert!(DDL.contains("principal   TEXT NOT NULL"));
        assert!(DDL.contains("created_at  BIGINT NOT NULL"));
        assert_eq!(TABLE, "firefly_session_registry");
    }

    #[test]
    fn index_ddl_targets_principal() {
        assert!(
            INDEX_DDL.contains("CREATE INDEX IF NOT EXISTS firefly_session_registry_principal_idx")
        );
        assert!(INDEX_DDL.contains("(principal)"));
    }

    #[test]
    fn upsert_is_on_conflict_do_update_on_session_id() {
        assert!(UPSERT_SQL.contains("ON CONFLICT (session_id) DO UPDATE"));
        assert!(UPSERT_SQL.contains("principal = EXCLUDED.principal"));
        assert!(UPSERT_SQL.contains("created_at = EXCLUDED.created_at"));
        assert!(
            UPSERT_SQL.contains("$1") && UPSERT_SQL.contains("$2") && UPSERT_SQL.contains("$3")
        );
    }

    #[test]
    fn delete_is_scoped_to_principal_and_session() {
        assert!(DELETE_SQL.contains("WHERE principal = $1 AND session_id = $2"));
    }

    #[test]
    fn list_orders_oldest_first() {
        assert!(LIST_SQL.contains("ORDER BY created_at ASC"));
        assert!(LIST_SQL.contains("WHERE principal = $1"));
        assert!(LIST_SQL.contains("SELECT session_id, created_at"));
    }

    #[test]
    fn count_is_per_principal() {
        assert!(COUNT_SQL.contains("COUNT(*)"));
        assert!(COUNT_SQL.contains("WHERE principal = $1"));
    }

    #[test]
    fn default_table_sql_matches_public_consts() {
        // Backward-compat guard: the rendered statements for the default TABLE
        // are byte-for-byte the public consts.
        let sql = TableSql::new(TABLE);
        assert_eq!(sql.table, TABLE);
        assert_eq!(sql.ddl, DDL);
        assert_eq!(sql.index_ddl, INDEX_DDL);
        assert_eq!(sql.upsert, UPSERT_SQL);
        assert_eq!(sql.delete, DELETE_SQL);
        assert_eq!(sql.list, LIST_SQL);
        assert_eq!(sql.count, COUNT_SQL);
    }

    #[test]
    fn custom_table_sql_targets_the_given_table() {
        let sql = TableSql::new("fftest_sess_demo");
        assert!(sql
            .ddl
            .contains("CREATE TABLE IF NOT EXISTS fftest_sess_demo"));
        assert!(sql
            .index_ddl
            .contains("fftest_sess_demo_principal_idx ON fftest_sess_demo"));
        assert!(sql.upsert.contains("INSERT INTO fftest_sess_demo"));
        assert!(sql.list.contains("FROM fftest_sess_demo"));
        assert!(sql.count.contains("FROM fftest_sess_demo"));
        // The DDL keeps the canonical column shape regardless of table name.
        assert!(sql.ddl.contains("session_id  TEXT PRIMARY KEY"));
        assert!(sql.ddl.contains("created_at  BIGINT NOT NULL"));
        // No leakage of the default table name.
        assert!(!sql.upsert.contains("firefly_session_registry"));
    }

    #[test]
    fn validate_table_name_accepts_plain_identifiers() {
        for ok in [
            "firefly_session_registry",
            "t",
            "_private",
            "fftest_sess_12345_0",
            "a1_b2_c3",
        ] {
            assert!(validate_table_name(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn validate_table_name_rejects_injection_and_bad_shapes() {
        for bad in [
            "x; DROP TABLE y", // the classic injection attempt
            "firefly session", // spaces
            "Mixed_Case",      // uppercase
            "9leading",        // leading digit
            "tbl;",            // statement terminator
            "tbl--",           // comment
            "tbl)",            // closing paren
            "schema.table",    // qualified name / dot
            "\"quoted\"",      // quotes
            "",                // empty
        ] {
            assert!(validate_table_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn validate_table_name_rejects_overlong_identifiers() {
        let long = "a".repeat(64);
        assert!(validate_table_name(&long).is_err());
        let max = "a".repeat(63);
        assert!(validate_table_name(&max).is_ok());
    }

    #[test]
    fn from_client_with_table_rejects_injection_name() {
        // The injection-y name must be rejected before any client is touched.
        assert!(build_table_sql("x; DROP TABLE y").is_err());
    }

    #[test]
    fn normalise_dsn_strips_dialect_markers() {
        assert_eq!(
            normalise_dsn("postgresql+asyncpg://u:p@h:5432/db"),
            "postgresql://u:p@h:5432/db"
        );
        assert_eq!(
            normalise_dsn("postgresql+psycopg://u:p@h/db"),
            "postgresql://u:p@h/db"
        );
        assert_eq!(
            normalise_dsn("postgres+asyncpg://u:p@h/db"),
            "postgresql://u:p@h/db"
        );
    }

    #[test]
    fn normalise_dsn_passes_plain_url_through() {
        assert_eq!(
            normalise_dsn("postgresql://u:p@h/db"),
            "postgresql://u:p@h/db"
        );
        assert_eq!(
            normalise_dsn("host=db user=app dbname=app"),
            "host=db user=app dbname=app"
        );
    }

    #[test]
    fn registry_is_object_safe_and_send_sync() {
        // The registry must compose behind `Arc<dyn SessionRegistry>` (how
        // `SessionConcurrencyController` holds it) — naming the type proves the
        // trait is object-safe. No live client is needed.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PostgresSessionRegistry>();
        let _erased: Option<std::sync::Arc<dyn SessionRegistry>> = None;
    }
}
