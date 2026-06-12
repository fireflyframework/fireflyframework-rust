//! Postgres advisory-lock [`DistributedLock`] adapter — pyfly
//! `pyfly.scheduling.adapters.postgres_lock` parity.
//!
//! Cluster-safe scheduled-task coordination with **no extra infrastructure**
//! for apps already on Postgres — uses `pg_try_advisory_lock` /
//! `pg_advisory_unlock`. Session-level advisory locks are tied to the
//! holding connection, so the connection opened in `try_acquire` is **held**
//! until `release` (or until the process dies — Postgres then drops the
//! connection and auto-releases the lock, which is the crash-safety
//! mechanism in lieu of a TTL; the `ttl` argument is accepted and ignored).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::lock::{DistributedLock, LockError};

/// A held advisory lock: the client whose session owns it. Dropping the
/// client closes the connection, which releases the lock server-side.
struct Held {
    client: tokio_postgres::Client,
}

/// Distributed lock backed by Postgres session-level advisory locks —
/// pyfly's `PostgresAdvisoryLock` over `tokio-postgres` instead of an
/// injected SQLAlchemy engine.
///
/// Construct with a [`tokio_postgres`] connection string (e.g.
/// `host=db user=app password=s3cret dbname=app` or a `postgres://` URL);
/// each `try_acquire` dials a dedicated connection that lives for as long as
/// the lock is held.
pub struct PostgresAdvisoryLock {
    config: String,
    held: tokio::sync::Mutex<HashMap<String, Held>>,
}

impl PostgresAdvisoryLock {
    /// Builds the lock from a `tokio-postgres` connection string. No
    /// connection is made until the first [`DistributedLock::try_acquire`].
    pub fn new(config: impl Into<String>) -> Self {
        Self {
            config: config.into(),
            held: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Maps a lock name to a stable signed 64-bit advisory-lock key,
    /// deterministic across processes (a fixed hash, not a per-process
    /// salted one).
    ///
    /// pyfly derives the key from the first 8 bytes of `blake2b`; this port
    /// uses the first 8 bytes of SHA-256 (big-endian, signed) because
    /// blake2 is not in the workspace dependency catalog — the *property*
    /// (stable cross-process i64) is identical, the key values are not, so
    /// do not mix pyfly and Rust instances on the same lock names.
    pub fn advisory_key(name: &str) -> i64 {
        let digest = Sha256::digest(name.as_bytes());
        let bytes: [u8; 8] = digest[..8].try_into().expect("sha256 is 32 bytes");
        i64::from_be_bytes(bytes)
    }
}

impl std::fmt::Debug for PostgresAdvisoryLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresAdvisoryLock")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl DistributedLock for PostgresAdvisoryLock {
    async fn try_acquire(&self, name: &str, _ttl: Duration) -> Result<bool, LockError> {
        let key = Self::advisory_key(name);
        let (client, connection) =
            tokio_postgres::connect(&self.config, tokio_postgres::NoTls).await?;
        // Drive the connection until the client is dropped.
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!(err = %err, "postgres advisory-lock connection closed");
            }
        });
        let row = client
            .query_one("SELECT pg_try_advisory_lock($1)", &[&key])
            .await?;
        let acquired: bool = row.get(0);
        if !acquired {
            // Dropping `client` closes the connection — nothing leaks when
            // the lock is held elsewhere.
            return Ok(false);
        }
        self.held
            .lock()
            .await
            .insert(name.to_string(), Held { client });
        Ok(true)
    }

    async fn release(&self, name: &str) -> Result<(), LockError> {
        let Some(held) = self.held.lock().await.remove(name) else {
            return Ok(()); // not held — no-op, as in pyfly
        };
        let key = Self::advisory_key(name);
        let result = held
            .client
            .query_one("SELECT pg_advisory_unlock($1)", &[&key])
            .await;
        // The client (and its connection) drops here regardless, which
        // releases the session lock server-side even if the query failed.
        result?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Port of pyfly test_key_is_deterministic_signed_64bit.
    #[test]
    fn key_is_deterministic_signed_64bit() {
        let k = PostgresAdvisoryLock::advisory_key("job");
        assert_eq!(PostgresAdvisoryLock::advisory_key("job"), k); // deterministic
        assert_ne!(PostgresAdvisoryLock::advisory_key("other-job"), k);
        // i64 by construction — the signed 64-bit range check is the type.
    }

    // Port of pyfly test_release_of_unheld_lock_is_noop: release of a never-
    // acquired name must not raise — and must not dial the database.
    #[tokio::test]
    async fn release_of_unheld_lock_is_noop() {
        let lock = PostgresAdvisoryLock::new("host=localhost user=nobody dbname=nothing");
        lock.release("never-acquired").await.unwrap();
    }

    // Port of pyfly test_satisfies_distributed_lock_protocol.
    #[test]
    fn satisfies_distributed_lock_trait_object() {
        let _lock: Arc<dyn DistributedLock> = Arc::new(PostgresAdvisoryLock::new("host=db"));
    }

    /// Reads the integration database URL from the standard env var, with the
    /// older `DATABASE_URL` / `POSTGRES_URL` fallbacks (tokio-postgres accepts
    /// the `postgres://` URL form directly). `None` ⇒ skip.
    fn pg_url() -> Option<String> {
        std::env::var("FIREFLY_TEST_POSTGRES_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .or_else(|_| std::env::var("POSTGRES_URL"))
            .ok()
    }

    /// Process-wide monotonic suffix source for collision-free per-test lock
    /// names — derived deterministically, not from a random source.
    static LOCK_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn unique_lock_name(slug: &str) -> String {
        let n = LOCK_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("fftest-{slug}-{}-{n}", std::process::id())
    }

    // Port of pyfly test_acquire_holds_connection_then_release_unlocks_and_
    // closes + test_acquire_failure_does_not_leak_connection, against a real
    // server (tokio-postgres has no injectable fake engine). Env-gated: reads
    // FIREFLY_TEST_POSTGRES_URL (fallback DATABASE_URL / POSTGRES_URL); a clean
    // skip when unset, a genuine pg_try_advisory_lock acquire / contend /
    // release round-trip when set. The lock name is unique per process + call,
    // and every acquired lock is released, so the test is idempotent and
    // parallel-safe.
    #[tokio::test]
    async fn acquire_release_round_trip_against_real_postgres() {
        let Some(dsn) = pg_url() else {
            eprintln!(
                "skipping acquire_release_round_trip_against_real_postgres: \
                 set FIREFLY_TEST_POSTGRES_URL to run"
            );
            return;
        };
        let name = unique_lock_name("job");
        let a = PostgresAdvisoryLock::new(&dsn);
        let b = PostgresAdvisoryLock::new(&dsn);
        let ttl = Duration::from_secs(30);
        // a acquires the session-held advisory lock.
        assert!(a.try_acquire(&name, ttl).await.unwrap());
        // b cannot acquire the same key while a holds it (separate session).
        assert!(!b.try_acquire(&name, ttl).await.unwrap());
        // a releases; the lock becomes available again.
        a.release(&name).await.unwrap();
        assert!(b.try_acquire(&name, ttl).await.unwrap());
        // Clean up so the suite is idempotent.
        b.release(&name).await.unwrap();
    }
}
