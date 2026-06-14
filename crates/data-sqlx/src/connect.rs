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

//! Config-driven connection — build a live [`Db`] pool (and, optionally,
//! register a transaction manager) from a URL plus pool settings.
//!
//! [`DataSourceProperties`] binds straight from a configuration source (it is a
//! plain `serde` struct, so `firefly_config::bind` / `serde` materialise it
//! from `firefly.datasource.*`), and [`Db::connect_with`] turns it into a
//! pool: the URL **scheme** selects the backend (`postgres://`,
//! `postgresql://`, `mysql://`, or `sqlite:`), and the pool sizing/timeout
//! fields map onto the matching `sqlx` pool options. [`auto_configure`] goes
//! one step further — it builds the pool and registers a
//! [`SqlxTransactionManager`](crate::SqlxTransactionManager) so `#[transactional]`
//! works without any manual wiring, the turn-key path an application calls once
//! at startup.
//!
//! This is the DI-free constructor half (the same split the framework uses for
//! the ECM and notifications adapters): the application owns config loading and
//! awaits these at boot; nothing here reaches into the container, and no driver
//! is forced on services that don't opt in.

use std::sync::Arc;
use std::time::Duration;

use firefly_kernel::FireflyError;
use serde::Deserialize;

use crate::db::Db;
use crate::tx::SqlxTransactionManager;

/// Connection + pool settings bound from configuration — the Firefly analog of
/// Spring Boot's `spring.datasource` + connection-pool properties.
///
/// Bind it from any prefix, e.g. `firefly.datasource.*`:
///
/// ```yaml
/// firefly:
///   datasource:
///     url: "postgres://user:pw@localhost/orders"
///     max-connections: 16
///     acquire-timeout-ms: 5000
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DataSourceProperties {
    /// The database URL. The scheme selects the backend: `postgres://` /
    /// `postgresql://` → PostgreSQL, `mysql://` → MySQL, `sqlite:` /
    /// `sqlite://` → SQLite.
    pub url: String,
    /// Maximum pool connections; `0` leaves the `sqlx` default.
    pub max_connections: u32,
    /// Minimum idle pool connections to maintain.
    pub min_connections: u32,
    /// Connection-acquire timeout in milliseconds; `0` leaves the default.
    pub acquire_timeout_ms: u64,
    /// Idle timeout in milliseconds before a connection is reaped; `0` = none.
    pub idle_timeout_ms: u64,
    /// Maximum connection lifetime in milliseconds; `0` = unbounded.
    pub max_lifetime_ms: u64,
}

impl DataSourceProperties {
    /// Properties for `url` with default pool settings.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    /// Sets the maximum pool size.
    pub fn max_connections(mut self, n: u32) -> Self {
        self.max_connections = n;
        self
    }

    /// Sets the minimum idle pool size.
    pub fn min_connections(mut self, n: u32) -> Self {
        self.min_connections = n;
        self
    }
}

impl Db {
    /// Connects a pool from `url` with default pool settings — the scheme
    /// selects the backend. Convenience wrapper over [`Db::connect_with`].
    pub async fn connect(url: &str) -> Result<Db, FireflyError> {
        Db::connect_with(&DataSourceProperties::new(url)).await
    }

    /// Connects a pool from [`DataSourceProperties`], applying the pool
    /// sizing/timeout settings and selecting the backend from the URL scheme.
    pub async fn connect_with(props: &DataSourceProperties) -> Result<Db, FireflyError> {
        let url = props.url.trim();
        if url.is_empty() {
            return Err(FireflyError::internal(
                "data: empty datasource url (set firefly.datasource.url)",
            ));
        }
        let scheme = url
            .split([':', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match scheme.as_str() {
            "postgres" | "postgresql" => connect_postgres(props).await,
            "mysql" => connect_mysql(props).await,
            "sqlite" => connect_sqlite(props).await,
            other => Err(FireflyError::internal(format!(
                "data: unsupported datasource url scheme {other:?}; expected postgres://, \
                 mysql://, or sqlite:"
            ))),
        }
    }
}

/// Builds a [`Db`] and registers a [`SqlxTransactionManager`] over it so
/// `#[transactional]` resolves without manual wiring — the one-call startup
/// path (the Rust analog of Spring Boot's `DataSourceTransactionManager`
/// auto-configuration). The returned [`Db`] is also handed back so the
/// application can build typed repositories from the same pool.
///
/// Transaction-manager registration is process-global and first-wins (a
/// manager the application registered earlier is preserved).
pub async fn auto_configure(props: &DataSourceProperties) -> Result<Db, FireflyError> {
    let db = Db::connect_with(props).await?;
    let manager = SqlxTransactionManager::new(db.clone());
    firefly_transactional::register_transaction_manager(
        Arc::new(manager) as Arc<dyn firefly_transactional::TransactionManager>
    );
    Ok(db)
}

#[cfg(feature = "postgres")]
async fn connect_postgres(props: &DataSourceProperties) -> Result<Db, FireflyError> {
    let mut opts = sqlx::postgres::PgPoolOptions::new();
    opts = apply_pg(opts, props);
    let pool = opts
        .connect(&props.url)
        .await
        .map_err(|e| FireflyError::internal(format!("data: postgres connect failed: {e}")))?;
    Ok(Db::Postgres(pool))
}

#[cfg(not(feature = "postgres"))]
async fn connect_postgres(_props: &DataSourceProperties) -> Result<Db, FireflyError> {
    Err(FireflyError::internal(
        "data: postgres backend not compiled in (enable the `postgres` feature)",
    ))
}

#[cfg(feature = "mysql")]
async fn connect_mysql(props: &DataSourceProperties) -> Result<Db, FireflyError> {
    let mut opts = sqlx::mysql::MySqlPoolOptions::new();
    opts = apply_mysql(opts, props);
    let pool = opts
        .connect(&props.url)
        .await
        .map_err(|e| FireflyError::internal(format!("data: mysql connect failed: {e}")))?;
    Ok(Db::MySql(pool))
}

#[cfg(not(feature = "mysql"))]
async fn connect_mysql(_props: &DataSourceProperties) -> Result<Db, FireflyError> {
    Err(FireflyError::internal(
        "data: mysql backend not compiled in (enable the `mysql` feature)",
    ))
}

#[cfg(feature = "sqlite")]
async fn connect_sqlite(props: &DataSourceProperties) -> Result<Db, FireflyError> {
    let mut opts = sqlx::sqlite::SqlitePoolOptions::new();
    opts = apply_sqlite(opts, props);
    let pool = opts
        .connect(&props.url)
        .await
        .map_err(|e| FireflyError::internal(format!("data: sqlite connect failed: {e}")))?;
    Ok(Db::Sqlite(pool))
}

#[cfg(not(feature = "sqlite"))]
async fn connect_sqlite(_props: &DataSourceProperties) -> Result<Db, FireflyError> {
    Err(FireflyError::internal(
        "data: sqlite backend not compiled in (enable the `sqlite` feature)",
    ))
}

// One pool-options applicator per backend: the option types are distinct, but
// the property mapping is identical (skip a setting when its field is 0).
macro_rules! apply_pool_opts {
    ($fn_name:ident, $opt_ty:ty) => {
        fn $fn_name(mut o: $opt_ty, props: &DataSourceProperties) -> $opt_ty {
            if props.max_connections > 0 {
                o = o.max_connections(props.max_connections);
            }
            o = o.min_connections(props.min_connections);
            if props.acquire_timeout_ms > 0 {
                o = o.acquire_timeout(Duration::from_millis(props.acquire_timeout_ms));
            }
            if props.idle_timeout_ms > 0 {
                o = o.idle_timeout(Duration::from_millis(props.idle_timeout_ms));
            }
            if props.max_lifetime_ms > 0 {
                o = o.max_lifetime(Duration::from_millis(props.max_lifetime_ms));
            }
            o
        }
    };
}

#[cfg(feature = "postgres")]
apply_pool_opts!(apply_pg, sqlx::postgres::PgPoolOptions);
#[cfg(feature = "mysql")]
apply_pool_opts!(apply_mysql, sqlx::mysql::MySqlPoolOptions);
#[cfg(feature = "sqlite")]
apply_pool_opts!(apply_sqlite, sqlx::sqlite::SqlitePoolOptions);

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::Backend;

    #[tokio::test]
    async fn connect_builds_a_pool_from_a_url() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect sqlite");
        assert_eq!(db.backend(), Backend::Sqlite);
    }

    #[tokio::test]
    async fn connect_with_applies_pool_settings_and_works() {
        let props = DataSourceProperties::new("sqlite::memory:").max_connections(3);
        let db = Db::connect_with(&props).await.expect("connect");
        assert_eq!(db.backend(), Backend::Sqlite);
        // Prove the pool is live.
        if let Db::Sqlite(pool) = &db {
            let one: i64 = sqlx::query_scalar("SELECT 1")
                .fetch_one(pool)
                .await
                .expect("query");
            assert_eq!(one, 1);
        }
    }

    #[tokio::test]
    async fn unknown_or_empty_scheme_errors() {
        assert!(Db::connect("redis://localhost").await.is_err());
        assert!(Db::connect("").await.is_err());
        assert!(Db::connect("not-a-url").await.is_err());
    }

    #[tokio::test]
    async fn auto_configure_builds_pool_and_registers_a_manager() {
        let db = auto_configure(&DataSourceProperties::new("sqlite::memory:"))
            .await
            .expect("auto-configure");
        assert_eq!(db.backend(), Backend::Sqlite);
        // A transaction manager is now globally resolvable (first-wins).
        assert!(firefly_transactional::transaction_manager().is_some());
    }

    #[test]
    fn properties_bind_from_a_config_document() {
        let props: DataSourceProperties = serde_json::from_value(serde_json::json!({
            "url": "postgres://localhost/db",
            "max_connections": 16,
            "acquire_timeout_ms": 5000
        }))
        .expect("bind");
        assert_eq!(props.url, "postgres://localhost/db");
        assert_eq!(props.max_connections, 16);
        assert_eq!(props.acquire_timeout_ms, 5000);
        // Unset fields fall back to their zero defaults.
        assert_eq!(props.min_connections, 0);
    }
}
