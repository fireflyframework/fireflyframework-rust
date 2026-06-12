//! Port of pyfly's `tests/security/test_persistent_token_store.py` —
//! the Redis and Postgres `TokenStore` adapters.
//!
//! pyfly drives a Python `_FakeRedis` object directly; the Rust
//! `RedisTokenStore` wraps a real `redis::aio::MultiplexedConnection`,
//! so the equivalent fixture is a minimal in-process RESP server (the
//! same shape `firefly-scheduling`'s `redis_lock` tests use) that
//! handles `SET key val [EX secs]`, `GET key`, and `DEL key`. The
//! Postgres round-trip is **env-gated** on `FIREFLY_TEST_POSTGRES_URL`
//! (fallback `DATABASE_URL` / `POSTGRES_URL`): it skips with a one-line
//! notice when unset and performs a genuine store / find / revoke cycle
//! against a real database when set; its table-name validation is covered
//! by the unit tests in `token_store.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use firefly_security::oauth2::{RedisTokenStore, TokenStore, REDIS_TOKEN_KEY_PREFIX};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// The fake server's view of the world: stored values keyed by key,
/// plus the TTL captured from the most recent `SET … EX` for each key.
#[derive(Default)]
struct FakeRedisState {
    values: HashMap<String, String>,
    ttls: HashMap<String, u64>,
}

type Store = Arc<Mutex<FakeRedisState>>;

/// Spawns a minimal in-process RESP server on port 0 — the Rust
/// spelling of pyfly's `_FakeRedis` (no time elapses in tests; TTLs are
/// recorded but never expire). Returns its `redis://` URL and the
/// shared store for assertions.
async fn spawn_fake_redis() -> (String, Store) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let store: Store = Arc::new(Mutex::new(FakeRedisState::default()));
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

/// Reads one RESP array-of-bulk-strings command; `None` on EOF/garbage.
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
            // SET key value [EX secs]
            let mut s = store.lock().unwrap();
            s.values.insert(args[1].clone(), args[2].clone());
            if let Some(pos) = args.iter().position(|a| a.eq_ignore_ascii_case("EX")) {
                if let Some(ttl) = args.get(pos + 1).and_then(|v| v.parse::<u64>().ok()) {
                    s.ttls.insert(args[1].clone(), ttl);
                }
            }
            "+OK\r\n".to_string()
        }
        Some("GET") => match store.lock().unwrap().values.get(&args[1]) {
            Some(v) => format!("${}\r\n{}\r\n", v.len(), v),
            None => "$-1\r\n".to_string(),
        },
        Some("DEL") => {
            let mut s = store.lock().unwrap();
            let removed = args[1..]
                .iter()
                .filter(|k| s.values.remove(*k).is_some())
                .count();
            format!(":{removed}\r\n")
        }
        Some("PING") => "+PONG\r\n".to_string(),
        // Connection-setup chatter (CLIENT SETINFO, HELLO, …) — accept.
        _ => "+OK\r\n".to_string(),
    }
}

// Ported from pyfly: test_redis_token_store_roundtrip_with_ttl
#[tokio::test]
async fn redis_token_store_roundtrip_with_ttl() {
    let (url, store) = spawn_fake_redis().await;
    let token_store = RedisTokenStore::connect(&url).await.unwrap().ttl(900);

    token_store
        .store("tok1", json!({"sub": "alice", "scope": "read"}))
        .await
        .unwrap();
    assert_eq!(
        token_store.find("tok1").await.unwrap(),
        Some(json!({"sub": "alice", "scope": "read"}))
    );

    // The refresh-token TTL is applied to the SET command.
    let key = format!("{REDIS_TOKEN_KEY_PREFIX}tok1");
    assert_eq!(store.lock().unwrap().ttls.get(&key), Some(&900));

    token_store.revoke("tok1").await.unwrap();
    assert_eq!(token_store.find("tok1").await.unwrap(), None);
}

#[tokio::test]
async fn redis_token_store_without_ttl_omits_expiry() {
    let (url, store) = spawn_fake_redis().await;
    let token_store = RedisTokenStore::connect(&url).await.unwrap();

    token_store.store("t", json!({"a": 1})).await.unwrap();
    assert_eq!(token_store.find("t").await.unwrap(), Some(json!({"a": 1})));

    let key = format!("{REDIS_TOKEN_KEY_PREFIX}t");
    assert_eq!(store.lock().unwrap().ttls.get(&key), None, "no EX sent");
}

#[tokio::test]
async fn redis_token_store_find_missing_returns_none() {
    let (url, _store) = spawn_fake_redis().await;
    let token_store = RedisTokenStore::connect(&url).await.unwrap();
    assert_eq!(token_store.find("missing").await.unwrap(), None);
}

#[tokio::test]
async fn redis_token_store_custom_key_prefix() {
    let (url, store) = spawn_fake_redis().await;
    let token_store = RedisTokenStore::connect(&url)
        .await
        .unwrap()
        .key_prefix("custom:");

    token_store.store("x", json!({"v": 1})).await.unwrap();
    assert!(store.lock().unwrap().values.contains_key("custom:x"));
}

#[tokio::test]
async fn redis_token_store_revoke_missing_is_noop() {
    let (url, _store) = spawn_fake_redis().await;
    let token_store = RedisTokenStore::connect(&url).await.unwrap();
    // DEL of an absent key must not error.
    token_store.revoke("ghost").await.unwrap();
}

/// Process-wide monotonic suffix source for collision-free per-test table
/// names — derived deterministically, not from a random source.
static PG_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Reads the integration database URL from the standard env var, with the
/// older `DATABASE_URL` / `POSTGRES_URL` fallbacks (tokio-postgres accepts the
/// `postgres://` URL form directly). `None` ⇒ skip.
fn pg_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_POSTGRES_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .or_else(|_| std::env::var("POSTGRES_URL"))
        .ok()
}

// Postgres round-trip needs a real database; the table-name guard itself is
// unit-tested in `token_store.rs`. Env-gated: reads FIREFLY_TEST_POSTGRES_URL
// (fallback DATABASE_URL / POSTGRES_URL); a clean skip when unset, a genuine
// store / find / revoke round-trip against a live server when set. The backing
// table is uniquely named per process + call and dropped afterwards, so the
// test is idempotent and parallel-safe.
#[tokio::test]
async fn postgres_token_store_roundtrip() {
    use firefly_security::oauth2::PostgresTokenStore;

    let Some(conn_str) = pg_url() else {
        eprintln!("skipping postgres_token_store_roundtrip: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };

    let n = PG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let table = format!("fftest_tokens_{}_{n}", std::process::id());
    let store = PostgresTokenStore::with_table(&conn_str, &table)
        .expect("generated table name is a valid identifier");

    store
        .store("pg-tok", json!({"client_id": "c1", "scope": "read"}))
        .await
        .unwrap();
    assert_eq!(
        store.find("pg-tok").await.unwrap(),
        Some(json!({"client_id": "c1", "scope": "read"}))
    );
    // Revoking an unknown token is a no-op.
    store.revoke("does-not-exist").await.unwrap();
    store.revoke("pg-tok").await.unwrap();
    assert_eq!(store.find("pg-tok").await.unwrap(), None);

    // Clean up the per-test table so the suite is idempotent.
    drop_test_table(&conn_str, &table).await;
}

/// Best-effort `DROP TABLE` for the per-test token table.
async fn drop_test_table(conn_str: &str, table: &str) {
    let Ok((client, connection)) = tokio_postgres::connect(conn_str, tokio_postgres::NoTls).await
    else {
        return;
    };
    tokio::spawn(async move {
        let _ = connection.await;
    });
    // `table` is a validated identifier (constructed via with_table above).
    let _ = client
        .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
        .await;
}
