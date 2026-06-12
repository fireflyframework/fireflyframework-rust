//! OAuth2 token stores — the [`TokenStore`] port plus in-memory,
//! Redis, and Postgres adapters (pyfly:
//! `pyfly.security.oauth2.authorization_server.TokenStore` +
//! `pyfly.security.adapters.{redis,postgres}_token_store`).

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{OnceCell, RwLock};

use super::authorization_server::OAuth2Error;

/// Port for storing and retrieving OAuth2 tokens (refresh tokens).
/// Token payloads are JSON objects (`client_id`, `scope`, `exp`).
#[async_trait]
pub trait TokenStore: Send + Sync {
    /// Persists `token_data` under `token_id` (upsert).
    async fn store(&self, token_id: &str, token_data: Value) -> Result<(), OAuth2Error>;

    /// Returns the token data for `token_id`, or `None` when unknown.
    async fn find(&self, token_id: &str) -> Result<Option<Value>, OAuth2Error>;

    /// Removes `token_id`; revoking an unknown token is a no-op.
    async fn revoke(&self, token_id: &str) -> Result<(), OAuth2Error>;
}

/// In-memory token store — suitable for development and testing.
#[derive(Debug, Default)]
pub struct InMemoryTokenStore {
    tokens: RwLock<HashMap<String, Value>>,
}

impl InMemoryTokenStore {
    /// Returns an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TokenStore for InMemoryTokenStore {
    async fn store(&self, token_id: &str, token_data: Value) -> Result<(), OAuth2Error> {
        self.tokens
            .write()
            .await
            .insert(token_id.to_string(), token_data);
        Ok(())
    }

    async fn find(&self, token_id: &str) -> Result<Option<Value>, OAuth2Error> {
        Ok(self.tokens.read().await.get(token_id).cloned())
    }

    async fn revoke(&self, token_id: &str) -> Result<(), OAuth2Error> {
        self.tokens.write().await.remove(token_id);
        Ok(())
    }
}

/// Wraps a backend failure as a `TOKEN_STORE_ERROR`.
fn store_error(detail: impl std::fmt::Display) -> OAuth2Error {
    OAuth2Error::new("TOKEN_STORE_ERROR", format!("token store: {detail}"))
}

// ---------------------------------------------------------------------------
// Redis adapter
// ---------------------------------------------------------------------------

/// Default key prefix for Redis-stored tokens.
pub const REDIS_TOKEN_KEY_PREFIX: &str = "firefly:oauth2:token:";

/// Redis-backed [`TokenStore`]: cross-instance refresh-token
/// persistence + fast distributed revocation for a multi-instance
/// authorization server. Tokens are stored as JSON strings with an
/// optional TTL (typically the refresh-token lifetime) so expired
/// tokens self-evict.
///
/// The pyfly adapter keys tokens as `pyfly:oauth2:token:<id>`; this
/// port deliberately brands the default prefix
/// [`REDIS_TOKEN_KEY_PREFIX`] — use
/// [`key_prefix`](RedisTokenStore::key_prefix) for interop with
/// another port's deployment.
#[derive(Clone)]
pub struct RedisTokenStore {
    conn: redis::aio::MultiplexedConnection,
    ttl: Option<u64>,
    prefix: String,
}

impl std::fmt::Debug for RedisTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisTokenStore")
            .field("ttl", &self.ttl)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl RedisTokenStore {
    /// Wraps an already-established multiplexed connection (hexagonal:
    /// the composition root owns connection setup).
    pub fn new(conn: redis::aio::MultiplexedConnection) -> Self {
        Self {
            conn,
            ttl: None,
            prefix: REDIS_TOKEN_KEY_PREFIX.to_string(),
        }
    }

    /// Connects to `url` (`redis://host:port`) and wraps the
    /// connection.
    pub async fn connect(url: &str) -> Result<Self, OAuth2Error> {
        let client = redis::Client::open(url).map_err(store_error)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(store_error)?;
        Ok(Self::new(conn))
    }

    /// Sets the per-token TTL in seconds (typically the refresh-token
    /// lifetime); `None` stores tokens without expiry.
    pub fn ttl(mut self, seconds: u64) -> Self {
        self.ttl = Some(seconds);
        self
    }

    /// Overrides the key prefix (default [`REDIS_TOKEN_KEY_PREFIX`]).
    pub fn key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    fn key(&self, token_id: &str) -> String {
        format!("{}{token_id}", self.prefix)
    }
}

#[async_trait]
impl TokenStore for RedisTokenStore {
    async fn store(&self, token_id: &str, token_data: Value) -> Result<(), OAuth2Error> {
        let payload = token_data.to_string();
        let mut conn = self.conn.clone();
        let mut cmd = redis::cmd("SET");
        cmd.arg(self.key(token_id)).arg(payload);
        if let Some(ttl) = self.ttl {
            cmd.arg("EX").arg(ttl);
        }
        cmd.query_async::<()>(&mut conn).await.map_err(store_error)
    }

    async fn find(&self, token_id: &str) -> Result<Option<Value>, OAuth2Error> {
        let mut conn = self.conn.clone();
        let raw: Option<String> = redis::cmd("GET")
            .arg(self.key(token_id))
            .query_async(&mut conn)
            .await
            .map_err(store_error)?;
        match raw {
            Some(json) => Ok(Some(serde_json::from_str(&json).map_err(store_error)?)),
            None => Ok(None),
        }
    }

    async fn revoke(&self, token_id: &str) -> Result<(), OAuth2Error> {
        let mut conn = self.conn.clone();
        redis::cmd("DEL")
            .arg(self.key(token_id))
            .query_async::<()>(&mut conn)
            .await
            .map_err(store_error)
    }
}

// ---------------------------------------------------------------------------
// Postgres adapter
// ---------------------------------------------------------------------------

/// Default table name for Postgres-stored tokens.
pub const POSTGRES_TOKEN_TABLE: &str = "firefly_oauth2_tokens";

/// Validates a SQL identifier the way pyfly's adapter does
/// (`^[A-Za-z_][A-Za-z0-9_]*$`); rejects anything that could smuggle
/// SQL into the interpolated table name.
pub fn validate_table_name(name: &str) -> Result<(), OAuth2Error> {
    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(OAuth2Error::new(
            "INVALID_TABLE",
            format!("Invalid token-store table name: {name:?}"),
        ))
    }
}

/// Postgres table-backed [`TokenStore`]: durable, auditable
/// refresh-token storage + cross-instance revocation, with no Redis
/// required. The backing table (`token_id TEXT PRIMARY KEY, data TEXT
/// NOT NULL`) is created lazily and idempotently on first use, and the
/// connection itself is established lazily from the configured
/// connection string (pyfly's lazy `engine_factory`).
///
/// Connections use `NoTls`; front a TLS-terminating pooler when
/// transport security is required.
pub struct PostgresTokenStore {
    conn_str: String,
    table: String,
    client: OnceCell<tokio_postgres::Client>,
}

impl std::fmt::Debug for PostgresTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresTokenStore")
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

impl PostgresTokenStore {
    /// Builds a store over `conn_str`
    /// (`host=... user=... dbname=...` or a `postgres://` URL) with
    /// the default [`POSTGRES_TOKEN_TABLE`] table.
    pub fn new(conn_str: impl Into<String>) -> Self {
        Self::with_table(conn_str, POSTGRES_TOKEN_TABLE).expect("default table name is valid")
    }

    /// Builds a store with a custom table name; fails fast on invalid
    /// identifiers (pyfly: `ValueError("Invalid token-store table
    /// name")`).
    pub fn with_table(
        conn_str: impl Into<String>,
        table: impl Into<String>,
    ) -> Result<Self, OAuth2Error> {
        let table = table.into();
        validate_table_name(&table)?;
        Ok(Self {
            conn_str: conn_str.into(),
            table,
            client: OnceCell::new(),
        })
    }

    /// Lazily connects, spawns the connection driver, and ensures the
    /// backing table exists.
    async fn client(&self) -> Result<&tokio_postgres::Client, OAuth2Error> {
        self.client
            .get_or_try_init(|| async {
                let (client, connection) =
                    tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
                        .await
                        .map_err(store_error)?;
                tokio::spawn(async move {
                    // The driver finishes when the client is dropped.
                    let _ = connection.await;
                });
                client
                    .execute(
                        &format!(
                            "CREATE TABLE IF NOT EXISTS {} \
                             (token_id TEXT PRIMARY KEY, data TEXT NOT NULL)",
                            self.table
                        ),
                        &[],
                    )
                    .await
                    .map_err(store_error)?;
                Ok(client)
            })
            .await
    }
}

#[async_trait]
impl TokenStore for PostgresTokenStore {
    async fn store(&self, token_id: &str, token_data: Value) -> Result<(), OAuth2Error> {
        let client = self.client().await?;
        client
            .execute(
                &format!(
                    "INSERT INTO {} (token_id, data) VALUES ($1, $2) \
                     ON CONFLICT (token_id) DO UPDATE SET data = EXCLUDED.data",
                    self.table
                ),
                &[&token_id, &token_data.to_string()],
            )
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn find(&self, token_id: &str) -> Result<Option<Value>, OAuth2Error> {
        let client = self.client().await?;
        let row = client
            .query_opt(
                &format!("SELECT data FROM {} WHERE token_id = $1", self.table),
                &[&token_id],
            )
            .await
            .map_err(store_error)?;
        match row {
            Some(row) => {
                let data: String = row.get(0);
                Ok(Some(serde_json::from_str(&data).map_err(store_error)?))
            }
            None => Ok(None),
        }
    }

    async fn revoke(&self, token_id: &str) -> Result<(), OAuth2Error> {
        let client = self.client().await?;
        client
            .execute(
                &format!("DELETE FROM {} WHERE token_id = $1", self.table),
                &[&token_id],
            )
            .await
            .map_err(store_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from pyfly: test_postgres_token_store_rejects_bad_table
    #[test]
    fn postgres_store_rejects_bad_table_names() {
        for bad in ["t; DROP TABLE x", "", "1abc", "a-b", "a b", "x\"y"] {
            let err = PostgresTokenStore::with_table("host=localhost", bad).unwrap_err();
            assert!(err.message.contains("table name"), "{bad}: {err}");
            assert_eq!(err.code, "INVALID_TABLE");
        }
    }

    #[test]
    fn postgres_store_accepts_valid_table_names() {
        for good in ["firefly_oauth2_tokens", "_t", "T9", "a_b_c"] {
            assert!(PostgresTokenStore::with_table("host=localhost", good).is_ok());
        }
    }
}
