//! Port of pyfly's `tests/security/test_persistent_token_store.py` —
//! the Redis and Postgres `TokenStore` adapters.
//!
//! pyfly drives a Python `_FakeRedis` object directly; the Rust
//! `RedisTokenStore` wraps a real `redis::aio::MultiplexedConnection`,
//! so the equivalent fixture is a minimal in-process RESP server (the
//! same shape `firefly-scheduling`'s `redis_lock` tests use) that
//! handles `SET key val [EX secs]`, `GET key`, and `DEL key`. The
//! Postgres round-trip is gated behind `#[ignore]` because it needs a
//! real database; its table-name validation is covered by the unit
//! tests in `token_store.rs`.

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

// Postgres round-trip needs a real database; the table-name guard
// itself is unit-tested in `token_store.rs`. This exercises the full
// store/find/revoke cycle against a live server when one is available.
#[tokio::test]
#[ignore = "requires a running Postgres on localhost:5432"]
async fn postgres_token_store_roundtrip() {
    use firefly_security::oauth2::PostgresTokenStore;

    let conn_str = "host=localhost user=postgres password=postgres dbname=postgres";
    let store = PostgresTokenStore::new(conn_str);

    store
        .store("pg-tok", json!({"client_id": "c1", "scope": "read"}))
        .await
        .unwrap();
    assert_eq!(
        store.find("pg-tok").await.unwrap(),
        Some(json!({"client_id": "c1", "scope": "read"}))
    );
    store.revoke("pg-tok").await.unwrap();
    assert_eq!(store.find("pg-tok").await.unwrap(), None);
}
