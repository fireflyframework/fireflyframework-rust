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

//! The [`WalletPersistenceConfig`] `@Configuration` + its async repository bean.

use std::str::FromStr;

use firefly::data_sqlx::Db;
use firefly::prelude::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::repositories::wallet::v1::WalletRepository;

/// `CREATE TABLE` DDL for the SQLite default backend.
const SQLITE_DDL: &str = "CREATE TABLE IF NOT EXISTS wallets (\
    id TEXT PRIMARY KEY, account_number TEXT NOT NULL, owner TEXT NOT NULL, \
    balance INTEGER NOT NULL, currency TEXT NOT NULL, status TEXT NOT NULL, \
    version INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)";

/// `CREATE TABLE` DDL for the PostgreSQL backend (`DATABASE_URL=postgres://…`).
const POSTGRES_DDL: &str = "CREATE TABLE IF NOT EXISTS wallets (\
    id TEXT PRIMARY KEY, account_number TEXT NOT NULL, owner TEXT NOT NULL, \
    balance BIGINT NOT NULL, currency TEXT NOT NULL, status TEXT NOT NULL, \
    version BIGINT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)";

/// The `@Configuration` holder for the wallet datasource + repository.
#[derive(Configuration, Default)]
pub struct WalletPersistenceConfig;

#[firefly::bean]
impl WalletPersistenceConfig {
    /// The wallet repository bean — an **async factory**. It opens the
    /// connection pool and applies the schema with `await` (the framework
    /// resolves it during `Container::init_async_beans`, after the scan).
    /// Defaults to an in-memory SQLite database; honours `DATABASE_URL` for
    /// real PostgreSQL.
    #[bean]
    async fn wallet_repository(&self) -> WalletRepository {
        WalletRepository::new(connect_and_migrate().await)
    }
}

/// Opens the configured database, applies the `wallets` schema, and returns
/// the framework [`Db`] handle. Panics on a connection/migration failure —
/// fail-fast startup, surfaced through `init_async_beans`.
pub async fn connect_and_migrate() -> Db {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        // A named, shared-cache in-memory database: the pool's kept-alive
        // connection (min_connections = 1) holds the schema for the process.
        "sqlite:file:lumen_ledger?mode=memory&cache=shared".to_string()
    });

    if url.starts_with("postgres") {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .expect("connect to PostgreSQL (DATABASE_URL)");
        sqlx::query(POSTGRES_DDL)
            .execute(&pool)
            .await
            .expect("apply wallets schema (PostgreSQL)");
        Db::Postgres(pool)
    } else {
        let opts = SqliteConnectOptions::from_str(&url)
            .expect("parse SQLite connect options")
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(5)
            .connect_with(opts)
            .await
            .expect("open SQLite pool");
        sqlx::query(SQLITE_DDL)
            .execute(&pool)
            .await
            .expect("apply wallets schema (SQLite)");
        Db::Sqlite(pool)
    }
}
