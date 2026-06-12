//! firefly-cache-postgres — a PostgreSQL-backed [`firefly_cache::Adapter`].
//!
//! [`PostgresCacheAdapter`] is the Rust port of pyfly's
//! `PostgresCacheAdapter` (`pyfly.cache.adapters.postgres`). It implements the
//! full Firefly cache port — `get` / `set` / `delete` / `clear` / `name` /
//! `health_check` plus the P1 extension methods `set_if_absent` / `exists` /
//! `delete_prefix` / `stats` — over [`tokio_postgres`], backed by a single
//! key/value/expires_at table:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS firefly_cache_entries (
//!     cache_key   TEXT PRIMARY KEY,
//!     value       BYTEA NOT NULL,
//!     expires_at  TIMESTAMPTZ NULL
//! )
//! ```
//!
//! | Port method     | SQL                                                       |
//! |-----------------|-----------------------------------------------------------|
//! | `get`           | `SELECT value … WHERE cache_key = $1 AND (expires_at IS NULL OR expires_at > now)` |
//! | `set`           | `INSERT … ON CONFLICT (cache_key) DO UPDATE`              |
//! | `set_if_absent` | `INSERT … ON CONFLICT (cache_key) DO NOTHING` (rows affected) |
//! | `delete`        | `DELETE … WHERE cache_key = $1`                           |
//! | `exists`        | `SELECT 1 … WHERE cache_key = $1 AND (not expired)`       |
//! | `delete_prefix` | `DELETE … WHERE cache_key LIKE $1 ESCAPE '\'`             |
//! | `clear`         | `DELETE FROM firefly_cache_entries`                       |
//! | `stats`         | `SELECT COUNT(*) … (not expired)` + in-process counters   |
//! | `health_check`  | `SELECT 1`                                                |
//!
//! Like pyfly, expiry is enforced **lazily at read time** by an
//! `expires_at > now` predicate (no background sweeper); a `set` with a
//! [`Duration`] TTL stores `now + ttl` and a `None` TTL stores `NULL`
//! (persistent). Values cross the [`firefly_cache::Adapter`] port as raw
//! bytes, so JSON encoding lives in [`firefly_cache::Typed`] exactly as for
//! the in-process [`firefly_cache::MemoryAdapter`] and the Redis adapter —
//! the table is byte-transparent and therefore wire-compatible with every
//! sibling port.
//!
//! Unlike pyfly — whose adapter is handed an already-built SQLAlchemy
//! `AsyncEngine` and has explicit `start()`/`stop()` hooks — this adapter is
//! constructed from a [`tokio_postgres::Client`] (the DI entry point) or a
//! connection string, and runs the create-table-if-not-exists DDL on
//! [`PostgresCacheAdapter::init`]. There is no `stop`: the client's lifecycle
//! belongs to its owner.
//!
//! # Custom table names
//!
//! By default every statement targets the [`TABLE`] (`firefly_cache_entries`).
//! To point an adapter at a different table — e.g. to give two logical caches
//! their own storage, or to isolate parallel integration tests — construct it
//! with [`PostgresCacheAdapter::connect_with_table`] or
//! [`PostgresCacheAdapter::from_client_with_table`]. The table name is
//! validated strictly (ASCII `[a-z0-9_]`, must start with a letter or
//! underscore, at most 63 bytes — Postgres's identifier limit) and an invalid
//! name is rejected with [`CacheError::Backend`] rather than being
//! interpolated into SQL, so there is no injection surface.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::time::Duration;
//! use firefly_cache::{Adapter, Typed};
//! use firefly_cache_postgres::PostgresCacheAdapter;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let adapter = Arc::new(
//!     PostgresCacheAdapter::connect("postgresql://localhost/app").await?,
//! );
//! adapter.init().await?;
//! adapter.set("k", b"v", Some(Duration::from_secs(60))).await?;
//! assert_eq!(adapter.get("k").await?, b"v");
//! # Ok(())
//! # }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firefly_cache::{Adapter, CacheError, CacheStats};
use tokio_postgres::{Client, NoTls};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

/// The cache table name, matching pyfly's `pyfly_cache_entries` under the
/// Rust framework's `firefly_` prefix.
pub const TABLE: &str = "firefly_cache_entries";

/// The create-table-if-not-exists DDL run by
/// [`PostgresCacheAdapter::init`] — pyfly's `_DDL`. The key/value/expires_at
/// shape is identical (`TEXT` PK, `BYTEA` value, nullable `TIMESTAMPTZ`).
pub const DDL: &str = "CREATE TABLE IF NOT EXISTS firefly_cache_entries (\n    \
     cache_key   TEXT PRIMARY KEY,\n    \
     value       BYTEA NOT NULL,\n    \
     expires_at  TIMESTAMPTZ NULL\n)";

/// `INSERT … ON CONFLICT DO UPDATE` upsert — pyfly's `put`.
pub const UPSERT_SQL: &str = "INSERT INTO firefly_cache_entries (cache_key, value, expires_at) \
     VALUES ($1, $2, $3) \
     ON CONFLICT (cache_key) DO UPDATE \
     SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at";

/// `INSERT … ON CONFLICT DO NOTHING` conditional insert — pyfly's
/// `put_if_absent`. The row count (0 or 1) tells the caller whether the
/// write happened.
pub const INSERT_IF_ABSENT_SQL: &str =
    "INSERT INTO firefly_cache_entries (cache_key, value, expires_at) \
     VALUES ($1, $2, $3) \
     ON CONFLICT (cache_key) DO NOTHING";

/// Expiry-aware single-row read — pyfly's `get`.
pub const SELECT_SQL: &str = "SELECT value FROM firefly_cache_entries \
     WHERE cache_key = $1 AND (expires_at IS NULL OR expires_at > $2)";

/// Expiry-aware existence probe — pyfly's `exists`.
pub const EXISTS_SQL: &str = "SELECT 1 FROM firefly_cache_entries \
     WHERE cache_key = $1 AND (expires_at IS NULL OR expires_at > $2)";

/// Single-key delete — pyfly's `evict`.
pub const DELETE_SQL: &str = "DELETE FROM firefly_cache_entries WHERE cache_key = $1";

/// Prefix delete via `LIKE … ESCAPE '\'` — pyfly's `evict_by_prefix`.
pub const DELETE_PREFIX_SQL: &str =
    "DELETE FROM firefly_cache_entries WHERE cache_key LIKE $1 ESCAPE '\\'";

/// Truncate-all — pyfly's `clear`.
pub const CLEAR_SQL: &str = "DELETE FROM firefly_cache_entries";

/// Live-entry count (for `stats.size`) — pyfly's `get_stats` size query.
pub const COUNT_SQL: &str = "SELECT COUNT(*) FROM firefly_cache_entries \
     WHERE expires_at IS NULL OR expires_at > $1";

/// Expiry-aware key listing for [`PostgresCacheAdapter::keys`] — pyfly's
/// `get_keys`.
pub const SELECT_KEYS_SQL: &str = "SELECT cache_key FROM firefly_cache_entries \
     WHERE cache_key LIKE $1 ESCAPE '\\' AND (expires_at IS NULL OR expires_at > $2) \
     LIMIT $3";

/// A [`firefly_cache::Adapter`] backed by a single PostgreSQL key/value
/// table.
///
/// See the [crate docs](crate) for the SQL mapping. The adapter holds an
/// owned [`tokio_postgres::Client`] (`tokio-postgres`'s client is already
/// `Send + Sync` and pipelines concurrent queries internally, so — unlike
/// the Redis adapter — no `Mutex` is needed). Hit/miss/eviction counters are
/// kept in-process (atomic), exactly like pyfly's adapter; Postgres does not
/// expose per-adapter hit counts.
pub struct PostgresCacheAdapter {
    client: Client,
    /// The per-instance SQL, rendered once at construction from the validated
    /// table name. For the default table these are byte-for-byte the public
    /// [`DDL`] / [`UPSERT_SQL`] / … consts.
    sql: TableSql,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

/// The set of statements an adapter runs, rendered from a single validated
/// table name. Building these once (rather than `format!`-ing on every call)
/// keeps the hot path allocation-free and guarantees every statement targets
/// the same, already-validated identifier.
struct TableSql {
    table: String,
    ddl: String,
    upsert: String,
    insert_if_absent: String,
    select: String,
    exists: String,
    delete: String,
    delete_prefix: String,
    clear: String,
    count: String,
    select_keys: String,
}

impl TableSql {
    /// Renders the full statement set for `table`, which **must** already have
    /// passed [`validate_table_name`].
    fn new(table: &str) -> Self {
        Self {
            table: table.to_owned(),
            ddl: format!(
                "CREATE TABLE IF NOT EXISTS {table} (\n    \
                 cache_key   TEXT PRIMARY KEY,\n    \
                 value       BYTEA NOT NULL,\n    \
                 expires_at  TIMESTAMPTZ NULL\n)"
            ),
            upsert: format!(
                "INSERT INTO {table} (cache_key, value, expires_at) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (cache_key) DO UPDATE \
                 SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at"
            ),
            insert_if_absent: format!(
                "INSERT INTO {table} (cache_key, value, expires_at) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (cache_key) DO NOTHING"
            ),
            select: format!(
                "SELECT value FROM {table} \
                 WHERE cache_key = $1 AND (expires_at IS NULL OR expires_at > $2)"
            ),
            exists: format!(
                "SELECT 1 FROM {table} \
                 WHERE cache_key = $1 AND (expires_at IS NULL OR expires_at > $2)"
            ),
            delete: format!("DELETE FROM {table} WHERE cache_key = $1"),
            delete_prefix: format!("DELETE FROM {table} WHERE cache_key LIKE $1 ESCAPE '\\'"),
            clear: format!("DELETE FROM {table}"),
            count: format!(
                "SELECT COUNT(*) FROM {table} \
                 WHERE expires_at IS NULL OR expires_at > $1"
            ),
            select_keys: format!(
                "SELECT cache_key FROM {table} \
                 WHERE cache_key LIKE $1 ESCAPE '\\' AND (expires_at IS NULL OR expires_at > $2) \
                 LIMIT $3"
            ),
        }
    }
}

impl std::fmt::Debug for PostgresCacheAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresCacheAdapter")
            .field("table", &self.sql.table)
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl PostgresCacheAdapter {
    /// Connects to Postgres using `conn` (a `postgresql://` URL or a
    /// `tokio-postgres` keyword/value string), spawns the connection driver
    /// task, and returns a ready adapter. Does **not** create the table —
    /// call [`init`](PostgresCacheAdapter::init) once after construction.
    ///
    /// SQLAlchemy dialect markers (`postgresql+asyncpg://`,
    /// `postgresql+psycopg://`, `postgres+asyncpg://`) are stripped so a
    /// pyfly-style URL connects unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] if the connection string is malformed
    /// or the initial connection cannot be established.
    pub async fn connect(conn: &str) -> Result<Self, CacheError> {
        Self::connect_with_table(conn, TABLE).await
    }

    /// Like [`connect`](PostgresCacheAdapter::connect) but targets a custom
    /// `table` instead of the default [`TABLE`] — every statement
    /// (DDL/UPSERT/SELECT/EXISTS/DELETE/DELETE-prefix/CLEAR/COUNT/SELECT-KEYS)
    /// is rendered against it. Useful to give two logical caches separate
    /// storage or to isolate parallel integration tests.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] if `table` is not a valid, safe
    /// identifier (ASCII `[a-z0-9_]`, starting with a letter or underscore, at
    /// most 63 bytes) — the name is validated, never interpolated blindly — or
    /// if the connection string is malformed or the connection cannot be
    /// established.
    pub async fn connect_with_table(conn: &str, table: &str) -> Result<Self, CacheError> {
        let sql = build_table_sql(table)?;
        let dsn = normalise_dsn(conn);
        let (client, connection) = tokio_postgres::connect(&dsn, NoTls)
            .await
            .map_err(backend_err)?;
        // Drive the connection until the client is dropped.
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self::from_sql(client, sql))
    }

    /// Wraps an already-established [`tokio_postgres::Client`] — the
    /// dependency-injection entry point, paralleling pyfly's
    /// `PostgresCacheAdapter(engine)`. Does **not** create the table; call
    /// [`init`](PostgresCacheAdapter::init).
    #[must_use]
    pub fn from_client(client: Client) -> Self {
        // The default TABLE is a compile-time-valid identifier, so building its
        // SQL cannot fail.
        Self::from_sql(client, TableSql::new(TABLE))
    }

    /// Like [`from_client`](PostgresCacheAdapter::from_client) but targets a
    /// custom `table`.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] if `table` is not a valid, safe
    /// identifier (see [`connect_with_table`](PostgresCacheAdapter::connect_with_table)).
    pub fn from_client_with_table(client: Client, table: &str) -> Result<Self, CacheError> {
        Ok(Self::from_sql(client, build_table_sql(table)?))
    }

    /// Shared constructor: wraps `client` with an already-rendered statement
    /// set and zeroed counters.
    fn from_sql(client: Client, sql: TableSql) -> Self {
        Self {
            client,
            sql,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// The table this adapter targets (the default [`TABLE`] unless built with
    /// a `_with_table` constructor).
    #[must_use]
    pub fn table(&self) -> &str {
        &self.sql.table
    }

    /// Runs the create-table-if-not-exists DDL ([`DDL`] for the default table)
    /// — pyfly's `start`. Idempotent: safe to call more than once and on a
    /// table that already exists.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] on a transport / DDL failure.
    pub async fn init(&self) -> Result<(), CacheError> {
        self.client
            .batch_execute(&self.sql.ddl)
            .await
            .map_err(backend_err)
    }

    /// Returns up to `limit` non-expired keys matching the glob-style
    /// `pattern` (`*` / `?`) — pyfly's `get_keys(pattern, limit)`. The glob
    /// is translated to a SQL `LIKE` pattern via [`glob_to_like`]. A `limit`
    /// of `0` returns no keys.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] on a transport failure.
    pub async fn keys(&self, pattern: &str, limit: i64) -> Result<Vec<String>, CacheError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let like = glob_to_like(pattern);
        let now = Utc::now();
        let rows = self
            .client
            .query(&self.sql.select_keys, &[&like, &now, &limit])
            .await
            .map_err(backend_err)?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    /// Reports whether Postgres answers a trivial `SELECT 1` — the fail-soft
    /// analogue of [`Adapter::health_check`]; a failure is reported as
    /// `false` rather than an error so callers can degrade gracefully
    /// (pyfly's `is_available`).
    pub async fn is_available(&self) -> bool {
        self.ping().await.is_ok()
    }

    /// Issues `SELECT 1`, returning the transport error on failure.
    async fn ping(&self) -> Result<(), CacheError> {
        self.client
            .query_one("SELECT 1", &[])
            .await
            .map(|_| ())
            .map_err(backend_err)
    }
}

#[async_trait]
impl Adapter for PostgresCacheAdapter {
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError> {
        let now = Utc::now();
        let row = self
            .client
            .query_opt(&self.sql.select, &[&key, &now])
            .await
            .map_err(backend_err)?;
        match row {
            Some(r) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Ok(r.get::<_, Vec<u8>>(0))
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Err(CacheError::NotFound)
            }
        }
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError> {
        let expires_at = expires_at(ttl);
        self.client
            .execute(&self.sql.upsert, &[&key, &value, &expires_at])
            .await
            .map_err(backend_err)?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        let removed = self
            .client
            .execute(&self.sql.delete, &[&key])
            .await
            .map_err(backend_err)?;
        if removed > 0 {
            self.evictions.fetch_add(removed, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.client
            .execute(&self.sql.clear, &[])
            .await
            .map_err(backend_err)?;
        Ok(())
    }

    fn name(&self) -> String {
        "postgres".to_owned()
    }

    async fn health_check(&self) -> Result<(), CacheError> {
        self.ping().await
    }

    /// `INSERT … ON CONFLICT (cache_key) DO NOTHING` — pyfly's
    /// `put_if_absent`. Returns `true` when a row was inserted (the key was
    /// absent), `false` when an entry already existed (the rows-affected
    /// count is 0).
    ///
    /// Mirroring pyfly, this keeps the fast `DO NOTHING` path: an
    /// **expired** row still blocks the insert (the row physically exists,
    /// even though [`get`](Adapter::get) would treat it as a miss), so callers
    /// must not rely on `set_if_absent` overwriting a stale entry.
    async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool, CacheError> {
        let expires_at = expires_at(ttl);
        let inserted = self
            .client
            .execute(&self.sql.insert_if_absent, &[&key, &value, &expires_at])
            .await
            .map_err(backend_err)?;
        Ok(inserted > 0)
    }

    /// `SELECT 1 … WHERE cache_key = $1 AND (not expired)` — pyfly's
    /// `exists`.
    async fn exists(&self, key: &str) -> Result<bool, CacheError> {
        let now = Utc::now();
        let row = self
            .client
            .query_opt(&self.sql.exists, &[&key, &now])
            .await
            .map_err(backend_err)?;
        Ok(row.is_some())
    }

    /// `DELETE … WHERE cache_key LIKE $1 ESCAPE '\'` — pyfly's
    /// `evict_by_prefix`. The literal prefix is `LIKE`-escaped (so `%` / `_`
    /// in the prefix match literally) and a trailing `%` wildcard appended.
    /// Returns the number of rows removed.
    async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError> {
        let pattern = format!("{}%", like_escape(prefix));
        let removed = self
            .client
            .execute(&self.sql.delete_prefix, &[&pattern])
            .await
            .map_err(backend_err)?;
        self.evictions.fetch_add(removed, Ordering::Relaxed);
        Ok(removed)
    }

    /// `SELECT COUNT(*) … (not expired)` for `size`, plus the in-process
    /// hit/miss/eviction counters — pyfly's `get_stats`.
    async fn stats(&self) -> Option<CacheStats> {
        let now = Utc::now();
        let row = self.client.query_one(&self.sql.count, &[&now]).await.ok()?;
        let size: i64 = row.get(0);
        Some(CacheStats::from_counters(
            size.max(0) as u64,
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.evictions.load(Ordering::Relaxed),
        ))
    }
}

/// Converts an optional TTL into the absolute `expires_at` timestamp stored
/// in the table. A `None` or zero duration means no expiry (`None` → `NULL`),
/// matching the `firefly_cache` contract (`ttl <= 0` ⇒ persistent) and
/// pyfly's `ttl is None` branch; otherwise the value is `now + ttl`.
#[must_use]
pub fn expires_at(ttl: Option<Duration>) -> Option<DateTime<Utc>> {
    match ttl {
        Some(d) if !d.is_zero() => {
            let delta = chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX);
            // Saturate at the representable maximum rather than panicking on a
            // pathologically large TTL — a far-future expiry is effectively
            // "never expires", matching the persistent-entry intent.
            Some(
                Utc::now()
                    .checked_add_signed(delta)
                    .unwrap_or(DateTime::<Utc>::MAX_UTC),
            )
        }
        _ => None,
    }
}

/// Translates a glob pattern (`*` / `?`) into a SQL `LIKE` pattern
/// (`%` / `_`), escaping the `LIKE` metacharacters `%`, `_` and `\` so they
/// match literally — pyfly's `_glob_to_like`.
///
/// ```
/// use firefly_cache_postgres::glob_to_like;
/// assert_eq!(glob_to_like("foo*"), "foo%");
/// assert_eq!(glob_to_like("foo?bar"), "foo_bar");
/// assert_eq!(glob_to_like("100%"), r"100\%");
/// ```
#[must_use]
pub fn glob_to_like(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for ch in pattern.chars() {
        match ch {
            '*' => out.push('%'),
            '?' => out.push('_'),
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

/// Escapes the SQL `LIKE` metacharacters (`%`, `_`, `\`) in a **literal**
/// key prefix so `delete_prefix("a%b")` only removes keys that literally
/// begin with `a%b`, not any key containing an arbitrary run after `a`. The
/// trailing `%` wildcard is appended by the caller after escaping — pyfly's
/// `prefix.replace("%", …).replace("_", …) + "%"`.
#[must_use]
pub fn like_escape(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len());
    for ch in prefix.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Strips a SQLAlchemy dialect marker (`postgresql+asyncpg://`,
/// `postgresql+psycopg://`, `postgres+asyncpg://`) so a pyfly-style URL
/// connects through plain `tokio-postgres`. Connection strings without a
/// marker pass through unchanged — mirrors `firefly-eda-postgres`'s
/// `normalise_dsn`.
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

/// Validates a cache table name as a safe, plain SQL identifier so it can be
/// rendered into statements without an injection risk: ASCII lowercase
/// letters, digits and underscores only, a leading letter or underscore (not a
/// digit), and at most 63 bytes (Postgres's identifier length limit). Returns
/// the name on success, or [`CacheError::Backend`] describing the violation.
///
/// Only `[a-z0-9_]` is accepted — no quoting, no mixed case, no dots — so the
/// validated name is always usable bare and never needs escaping.
///
/// ```
/// use firefly_cache_postgres::validate_table_name;
/// assert!(validate_table_name("firefly_cache_entries").is_ok());
/// assert!(validate_table_name("fftest_cache_put_123").is_ok());
/// assert!(validate_table_name("x; DROP TABLE y").is_err());
/// assert!(validate_table_name("").is_err());
/// assert!(validate_table_name("9leading").is_err());
/// assert!(validate_table_name("Mixed").is_err());
/// ```
///
/// # Errors
///
/// Returns [`CacheError::Backend`] when the name is empty, too long, starts
/// with a digit, or contains any character outside `[a-z0-9_]`.
pub fn validate_table_name(table: &str) -> Result<&str, CacheError> {
    fn invalid(table: &str, why: &str) -> CacheError {
        CacheError::Backend(format!(
            "firefly/cache-postgres: invalid table name {table:?}: {why}"
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
fn build_table_sql(table: &str) -> Result<TableSql, CacheError> {
    validate_table_name(table).map(TableSql::new)
}

/// Wraps a [`tokio_postgres::Error`] as the cache port's
/// [`CacheError::Backend`].
fn backend_err(e: tokio_postgres::Error) -> CacheError {
    CacheError::Backend(format!("firefly/cache-postgres: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ----------------------------------------------------------------------
    // glob_to_like — pyfly tests/cache TestGlobToLike (faithful port).
    // ----------------------------------------------------------------------

    #[test]
    fn glob_star_becomes_percent() {
        assert_eq!(glob_to_like("foo*"), "foo%");
    }

    #[test]
    fn glob_question_mark_becomes_underscore() {
        assert_eq!(glob_to_like("foo?bar"), "foo_bar");
    }

    #[test]
    fn glob_literal_percent_is_escaped() {
        assert_eq!(glob_to_like("100%"), r"100\%");
    }

    #[test]
    fn glob_literal_underscore_is_escaped() {
        assert_eq!(glob_to_like("a_b"), r"a\_b");
    }

    #[test]
    fn glob_literal_backslash_is_escaped() {
        assert_eq!(glob_to_like(r"a\b"), r"a\\b");
    }

    #[test]
    fn glob_wildcard_only() {
        assert_eq!(glob_to_like("*"), "%");
    }

    #[test]
    fn glob_mixed() {
        assert_eq!(glob_to_like("pre:*:suf?"), "pre:%:suf_");
    }

    // ----------------------------------------------------------------------
    // like_escape — the delete_prefix literal-prefix escaping.
    // ----------------------------------------------------------------------

    #[test]
    fn like_escape_passes_plain_prefix() {
        assert_eq!(like_escape("p:"), "p:");
    }

    #[test]
    fn like_escape_escapes_metacharacters() {
        assert_eq!(like_escape("a%b_c"), r"a\%b\_c");
        assert_eq!(like_escape(r"x\y"), r"x\\y");
    }

    #[test]
    fn delete_prefix_pattern_appends_wildcard() {
        // The shape delete_prefix builds: escaped literal + trailing '%'.
        let pattern = format!("{}%", like_escape("100%:"));
        assert_eq!(pattern, r"100\%:%");
    }

    // ----------------------------------------------------------------------
    // expires_at — TTL → absolute-timestamp logic (pyfly's put branch).
    // ----------------------------------------------------------------------

    #[test]
    fn expires_at_none_for_no_ttl() {
        assert!(expires_at(None).is_none());
    }

    #[test]
    fn expires_at_none_for_zero_ttl() {
        // Zero TTL is persistent (firefly_cache `ttl <= 0` contract).
        assert!(expires_at(Some(Duration::ZERO)).is_none());
    }

    #[test]
    fn expires_at_in_the_future_for_positive_ttl() {
        let before = Utc::now();
        let exp = expires_at(Some(Duration::from_secs(60))).expect("some");
        // now + 60s lands between (before+~59s) and (before+~61s).
        assert!(exp > before + chrono::Duration::seconds(59));
        assert!(exp < before + chrono::Duration::seconds(61));
    }

    #[test]
    fn expires_at_handles_huge_ttl_without_panicking() {
        // Out-of-range std::Duration saturates rather than panicking.
        let exp = expires_at(Some(Duration::from_secs(u64::MAX)));
        assert!(exp.is_some());
    }

    // ----------------------------------------------------------------------
    // normalise_dsn — SQLAlchemy dialect-marker stripping.
    // ----------------------------------------------------------------------

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

    // ----------------------------------------------------------------------
    // SQL / DDL string shape — guards against drift in the prepared
    // statements and the table schema (the only invariants testable without
    // a live DB).
    // ----------------------------------------------------------------------

    #[test]
    fn ddl_has_the_pyfly_table_shape() {
        assert!(DDL.contains("CREATE TABLE IF NOT EXISTS firefly_cache_entries"));
        assert!(DDL.contains("cache_key   TEXT PRIMARY KEY"));
        assert!(DDL.contains("value       BYTEA NOT NULL"));
        assert!(DDL.contains("expires_at  TIMESTAMPTZ NULL"));
        assert_eq!(TABLE, "firefly_cache_entries");
    }

    #[test]
    fn upsert_is_on_conflict_do_update() {
        assert!(UPSERT_SQL.contains("ON CONFLICT (cache_key) DO UPDATE"));
        assert!(UPSERT_SQL.contains("value = EXCLUDED.value"));
        assert!(UPSERT_SQL.contains("expires_at = EXCLUDED.expires_at"));
        assert!(
            UPSERT_SQL.contains("$1") && UPSERT_SQL.contains("$2") && UPSERT_SQL.contains("$3")
        );
    }

    #[test]
    fn insert_if_absent_is_on_conflict_do_nothing() {
        assert!(INSERT_IF_ABSENT_SQL.contains("ON CONFLICT (cache_key) DO NOTHING"));
        assert!(!INSERT_IF_ABSENT_SQL.contains("DO UPDATE"));
    }

    #[test]
    fn select_and_exists_carry_the_expiry_predicate() {
        let predicate = "(expires_at IS NULL OR expires_at > $2)";
        assert!(SELECT_SQL.contains(predicate));
        assert!(EXISTS_SQL.contains(predicate));
        assert!(SELECT_SQL.contains("SELECT value"));
        assert!(EXISTS_SQL.contains("SELECT 1"));
    }

    #[test]
    fn delete_prefix_uses_like_with_escape() {
        assert!(DELETE_PREFIX_SQL.contains("LIKE $1 ESCAPE '\\'"));
        assert!(SELECT_KEYS_SQL.contains("LIKE $1 ESCAPE '\\'"));
        assert!(SELECT_KEYS_SQL.contains("LIMIT $3"));
    }

    #[test]
    fn count_query_excludes_expired_rows() {
        assert!(COUNT_SQL.contains("COUNT(*)"));
        assert!(COUNT_SQL.contains("expires_at IS NULL OR expires_at > $1"));
    }

    // ----------------------------------------------------------------------
    // Adapter object-safety — the port must compose behind Arc<dyn Adapter>.
    // ----------------------------------------------------------------------

    #[test]
    fn adapter_is_object_safe() {
        // `Arc<dyn Adapter>` is a well-formed type only when `Adapter` is
        // object-safe; naming it here (and exercising a dispatched method
        // through it) confirms the port composes behind a trait object
        // exactly as the framework relies on. No live client is needed.
        let erased: Option<Arc<dyn Adapter>> = None;
        let name = erased.as_ref().map(|a| a.name());
        assert!(name.is_none());
    }

    #[test]
    fn adapter_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PostgresCacheAdapter>();
    }

    // ----------------------------------------------------------------------
    // validate_table_name / custom-table SQL rendering — the configurable
    // table feature. No live DB is needed: validation and string rendering
    // are pure.
    // ----------------------------------------------------------------------

    #[test]
    fn validate_table_name_accepts_plain_identifiers() {
        for ok in [
            "firefly_cache_entries",
            "t",
            "_private",
            "fftest_cache_put_12345_0",
            "a1_b2_c3",
        ] {
            assert!(validate_table_name(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn validate_table_name_rejects_injection_and_bad_shapes() {
        for bad in [
            "x; DROP TABLE y",       // the classic injection attempt
            "firefly cache entries", // spaces
            "Mixed_Case",            // uppercase
            "9leading",              // leading digit
            "tbl;",                  // statement terminator
            "tbl--",                 // comment
            "tbl)",                  // closing paren
            "schema.table",          // qualified name / dot
            "\"quoted\"",            // quotes
            "",                      // empty
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
        // The injection-y name must be rejected before any client is touched,
        // so a None/placeholder client is never dereferenced. We exercise the
        // validation path directly via build_table_sql.
        assert!(build_table_sql("x; DROP TABLE y").is_err());
    }

    #[test]
    fn default_table_sql_matches_public_consts() {
        // Backward-compat guard: the rendered statements for the default TABLE
        // are byte-for-byte the long-standing public consts, so existing
        // callers and the const-shape unit tests above stay valid.
        let sql = TableSql::new(TABLE);
        assert_eq!(sql.table, TABLE);
        assert_eq!(sql.ddl, DDL);
        assert_eq!(sql.upsert, UPSERT_SQL);
        assert_eq!(sql.insert_if_absent, INSERT_IF_ABSENT_SQL);
        assert_eq!(sql.select, SELECT_SQL);
        assert_eq!(sql.exists, EXISTS_SQL);
        assert_eq!(sql.delete, DELETE_SQL);
        assert_eq!(sql.delete_prefix, DELETE_PREFIX_SQL);
        assert_eq!(sql.clear, CLEAR_SQL);
        assert_eq!(sql.count, COUNT_SQL);
        assert_eq!(sql.select_keys, SELECT_KEYS_SQL);
    }

    #[test]
    fn custom_table_sql_targets_the_given_table() {
        let sql = TableSql::new("fftest_cache_demo");
        assert!(sql
            .ddl
            .contains("CREATE TABLE IF NOT EXISTS fftest_cache_demo"));
        assert!(sql.upsert.contains("INSERT INTO fftest_cache_demo"));
        assert!(sql.select.contains("FROM fftest_cache_demo"));
        assert!(sql.clear.contains("DELETE FROM fftest_cache_demo"));
        // The DDL keeps the canonical column shape regardless of table name.
        assert!(sql.ddl.contains("cache_key   TEXT PRIMARY KEY"));
        assert!(sql.ddl.contains("value       BYTEA NOT NULL"));
        assert!(sql.ddl.contains("expires_at  TIMESTAMPTZ NULL"));
        // No leakage of the default table name.
        assert!(!sql.upsert.contains("firefly_cache_entries"));
    }
}
