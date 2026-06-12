//! Redis-backed [`DistributedLock`] adapter — pyfly
//! `pyfly.scheduling.adapters.redis_lock` parity.
//!
//! Acquire is an atomic `SET key token NX PX ttl_ms`; release is an
//! owner-token compare-and-delete Lua script (`EVAL`), so an instance only
//! releases a lock it still owns — never one that already expired and was
//! re-acquired elsewhere.

use std::time::Duration;

use async_trait::async_trait;
use redis::aio::MultiplexedConnection;

use crate::lock::{DistributedLock, LockError};

/// Owner-token compare-and-delete: release only if we still own the key,
/// atomically. Byte-for-byte the pyfly `_RELEASE_LUA` script.
const RELEASE_LUA: &str =
    "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end";

/// Default key prefix; pyfly uses `pyfly:schedlock:`, the Rust port brands
/// its own namespace.
const DEFAULT_PREFIX: &str = "firefly:schedlock:";

/// Distributed lock over a Redis server (`SET NX PX` + owner-token Lua
/// release) — pyfly's `RedisDistributedLock`.
///
/// The connection is multiplexed and established lazily on first use, so
/// constructing the lock never touches the network.
pub struct RedisLock {
    client: redis::Client,
    conn: tokio::sync::Mutex<Option<MultiplexedConnection>>,
    prefix: String,
    /// Per-instance owner token (random UUID hex), the release guard.
    token: String,
}

impl RedisLock {
    /// Wraps an existing [`redis::Client`] with the default
    /// `firefly:schedlock:` key prefix.
    pub fn new(client: redis::Client) -> Self {
        Self::with_prefix(client, DEFAULT_PREFIX)
    }

    /// Wraps an existing [`redis::Client`] with a custom key prefix.
    pub fn with_prefix(client: redis::Client, prefix: impl Into<String>) -> Self {
        Self {
            client,
            conn: tokio::sync::Mutex::new(None),
            prefix: prefix.into(),
            token: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    /// Convenience: builds the lock from a `redis://` connection URL.
    pub fn from_url(url: &str) -> Result<Self, LockError> {
        Ok(Self::new(redis::Client::open(url)?))
    }

    /// The namespaced key for a lock name.
    fn key(&self, name: &str) -> String {
        format!("{}{}", self.prefix, name)
    }

    /// Returns the cached multiplexed connection, dialing on first use.
    async fn connection(&self) -> Result<MultiplexedConnection, redis::RedisError> {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.as_ref() {
            return Ok(conn.clone());
        }
        let conn = self.client.get_multiplexed_async_connection().await?;
        *guard = Some(conn.clone());
        Ok(conn)
    }
}

impl std::fmt::Debug for RedisLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisLock")
            .field("prefix", &self.prefix)
            .finish()
    }
}

#[async_trait]
impl DistributedLock for RedisLock {
    async fn try_acquire(&self, name: &str, ttl: Duration) -> Result<bool, LockError> {
        let mut conn = self.connection().await?;
        // max(1) guards a sub-millisecond ttl yielding PX 0, which Redis
        // rejects — mirrors pyfly's max(1, int(ttl * 1000)).
        let px = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1);
        let reply: Option<String> = redis::cmd("SET")
            .arg(self.key(name))
            .arg(&self.token)
            .arg("NX")
            .arg("PX")
            .arg(px)
            .query_async(&mut conn)
            .await?;
        Ok(reply.is_some())
    }

    async fn release(&self, name: &str) -> Result<(), LockError> {
        let mut conn = self.connection().await?;
        let _deleted: i64 = redis::cmd("EVAL")
            .arg(RELEASE_LUA)
            .arg(1)
            .arg(self.key(name))
            .arg(&self.token)
            .query_async(&mut conn)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    type Store = Arc<Mutex<HashMap<String, String>>>;

    /// Minimal in-process RESP server: SET NX PX + the release-Lua
    /// compare-and-del — the Rust spelling of pyfly's `_FakeRedis` (no time
    /// elapses in tests; TTLs are accepted and ignored).
    async fn spawn_fake_redis() -> (String, Store) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store: Store = Arc::new(Mutex::new(HashMap::new()));
        let shared = Arc::clone(&store);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(serve_conn(sock, Arc::clone(&shared)));
            }
        });
        (format!("redis://{addr}"), store)
    }

    async fn serve_conn(sock: TcpStream, store: Store) {
        let (read, mut write) = sock.into_split();
        let mut reader = BufReader::new(read);
        loop {
            let Some(args) = read_command(&mut reader).await else {
                return;
            };
            let reply = respond(&args, &store);
            if write.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    /// Reads one RESP array-of-bulk-strings command; None on EOF/garbage.
    async fn read_command(
        reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    ) -> Option<Vec<String>> {
        let mut header = String::new();
        if reader.read_line(&mut header).await.ok()? == 0 {
            return None;
        }
        let n: usize = header.trim_end().strip_prefix('*')?.parse().ok()?;
        let mut args = Vec::with_capacity(n);
        for _ in 0..n {
            let mut len_line = String::new();
            if reader.read_line(&mut len_line).await.ok()? == 0 {
                return None;
            }
            let len: usize = len_line.trim_end().strip_prefix('$')?.parse().ok()?;
            let mut buf = vec![0u8; len + 2]; // payload + trailing \r\n
            reader.read_exact(&mut buf).await.ok()?;
            buf.truncate(len);
            args.push(String::from_utf8(buf).ok()?);
        }
        Some(args)
    }

    fn respond(args: &[String], store: &Store) -> String {
        match args.first().map(|c| c.to_ascii_uppercase()).as_deref() {
            Some("SET") => {
                // SET key value [NX] [PX ms]
                let nx = args.iter().any(|a| a.eq_ignore_ascii_case("NX"));
                let mut s = store.lock().unwrap();
                if nx && s.contains_key(&args[1]) {
                    "$-1\r\n".to_string() // nil: not acquired
                } else {
                    s.insert(args[1].clone(), args[2].clone());
                    "+OK\r\n".to_string()
                }
            }
            Some("EVAL") => {
                // EVAL script 1 key token → owner-token compare-and-del.
                let (key, token) = (&args[3], &args[4]);
                let mut s = store.lock().unwrap();
                if s.get(key) == Some(token) {
                    s.remove(key);
                    ":1\r\n".to_string()
                } else {
                    ":0\r\n".to_string()
                }
            }
            Some("GET") => match store.lock().unwrap().get(&args[1]) {
                Some(v) => format!("${}\r\n{}\r\n", v.len(), v),
                None => "$-1\r\n".to_string(),
            },
            Some("PING") => "+PONG\r\n".to_string(),
            // Connection-setup chatter (CLIENT SETINFO, …) — accept all.
            _ => "+OK\r\n".to_string(),
        }
    }

    // Port of pyfly test_redis_lock_acquire_release_cycle.
    #[tokio::test]
    async fn redis_lock_acquire_release_cycle() {
        let (url, _store) = spawn_fake_redis().await;
        let lock = RedisLock::from_url(&url).unwrap();
        let ttl = Duration::from_secs(30);
        assert!(lock.try_acquire("job", ttl).await.unwrap());
        assert!(!lock.try_acquire("job", ttl).await.unwrap()); // held (SET NX)
        lock.release("job").await.unwrap();
        assert!(lock.try_acquire("job", ttl).await.unwrap());
    }

    // Port of pyfly test_redis_lock_only_owner_can_release.
    #[tokio::test]
    async fn redis_lock_only_owner_can_release() {
        let (url, _store) = spawn_fake_redis().await;
        // Distinct owner tokens against the same server-side store.
        let a = RedisLock::from_url(&url).unwrap();
        let b = RedisLock::from_url(&url).unwrap();
        let ttl = Duration::from_secs(30);
        assert!(a.try_acquire("job", ttl).await.unwrap());
        assert!(!b.try_acquire("job", ttl).await.unwrap());
        b.release("job").await.unwrap(); // b doesn't own it -> no-op
        assert!(!b.try_acquire("job", ttl).await.unwrap()); // still a's
        a.release("job").await.unwrap(); // owner releases
        assert!(b.try_acquire("job", ttl).await.unwrap());
    }

    // Port of pyfly test_redis_lock_satisfies_protocol.
    #[tokio::test]
    async fn redis_lock_satisfies_trait_object() {
        let (url, _store) = spawn_fake_redis().await;
        let lock: Arc<dyn DistributedLock> = Arc::new(RedisLock::from_url(&url).unwrap());
        assert!(lock.try_acquire("j", Duration::from_secs(1)).await.unwrap());
    }

    // Wire-format checks the pyfly fake asserts implicitly: the key is
    // prefixed and the stored value is this instance's owner token.
    #[tokio::test]
    async fn redis_lock_key_prefix_and_token_on_the_wire() {
        let (url, store) = spawn_fake_redis().await;
        let lock = RedisLock::from_url(&url).unwrap();
        assert!(lock
            .try_acquire("job", Duration::from_secs(30))
            .await
            .unwrap());
        let stored = store.lock().unwrap().clone();
        assert_eq!(stored.len(), 1);
        assert!(stored.contains_key("firefly:schedlock:job"));
        assert_eq!(stored["firefly:schedlock:job"], lock.token);
    }
}
