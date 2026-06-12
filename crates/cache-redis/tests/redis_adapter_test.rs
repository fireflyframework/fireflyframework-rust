//! Behaviour-contract tests for [`firefly_cache_redis::RedisAdapter`],
//! ported from pyfly's `tests/cache/test_redis_adapter.py` (which runs
//! against an in-memory `FakeRedis` stub) and extended with the Rust port's
//! native verbs (`SET NX`, `SCAN MATCH` prefix deletion, `DBSIZE` stats,
//! the `keys` helper, `is_available`).
//!
//! Every test runs against an **in-process fake RESP server** ([`common`])
//! over a real TCP socket — no external Redis. A docker-gated round-trip
//! against a real Redis lives at the bottom, mirroring pyfly's
//! `tests/integration/test_cache_redis_integration.py`.

mod common;

use std::time::Duration;

use common::FakeRedis;
use firefly_cache::{Adapter, CacheError, Typed};
use firefly_cache_redis::RedisAdapter;

async fn adapter() -> (FakeRedis, RedisAdapter) {
    let fake = FakeRedis::start().await;
    let adapter = RedisAdapter::connect(&fake.url()).await.unwrap();
    (fake, adapter)
}

// ---------------------------------------------------------------------------
// pyfly: TestRedisCacheAdapter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_and_get() {
    // pyfly test_put_and_get (bytes-level here; JSON handled by Typed).
    let (_fake, a) = adapter().await;
    a.set("key", b"value", None).await.unwrap();
    assert_eq!(a.get("key").await.unwrap(), b"value");
}

#[tokio::test]
async fn get_missing_key_is_not_found() {
    // pyfly test_get_missing_key.
    let (_fake, a) = adapter().await;
    let err = a.get("no-such-key").await.unwrap_err();
    assert!(err.is_not_found(), "got {err}");
}

#[tokio::test]
async fn evict() {
    // pyfly test_evict.
    let (_fake, a) = adapter().await;
    a.set("key", b"value", None).await.unwrap();
    a.delete("key").await.unwrap();
    assert!(a.get("key").await.unwrap_err().is_not_found());
}

#[tokio::test]
async fn evict_missing_is_noop() {
    // pyfly test_evict_missing_returns_false — delete of an absent key is Ok.
    let (_fake, a) = adapter().await;
    a.delete("missing").await.unwrap();
}

#[tokio::test]
async fn exists() {
    // pyfly test_exists.
    let (_fake, a) = adapter().await;
    a.set("key", b"value", None).await.unwrap();
    assert!(a.exists("key").await.unwrap());
    assert!(!a.exists("missing").await.unwrap());
}

#[tokio::test]
async fn clear() {
    // pyfly test_clear -> FLUSHDB.
    let (_fake, a) = adapter().await;
    a.set("a", b"1", None).await.unwrap();
    a.set("b", b"2", None).await.unwrap();
    a.clear().await.unwrap();
    assert!(a.get("a").await.unwrap_err().is_not_found());
    assert!(a.get("b").await.unwrap_err().is_not_found());
}

#[tokio::test]
async fn put_with_ttl_forwards_px() {
    // pyfly test_put_with_ttl: the TTL reaches the client as a `PX` arg.
    let fake = FakeRedis::start().await;
    let a = RedisAdapter::connect(&fake.url()).await.unwrap();
    a.set("key", b"val", Some(Duration::from_secs(60)))
        .await
        .unwrap();
    assert_eq!(a.get("key").await.unwrap(), b"val");
    let px = fake.state.lock().unwrap().last_px.get("key").copied();
    assert_eq!(px, Some(60_000), "60s TTL must be forwarded as PX 60000");
}

#[tokio::test]
async fn no_ttl_does_not_set_px() {
    let fake = FakeRedis::start().await;
    let a = RedisAdapter::connect(&fake.url()).await.unwrap();
    a.set("key", b"val", None).await.unwrap();
    assert!(!fake.state.lock().unwrap().last_px.contains_key("key"));
    // A zero TTL is also "no expiry".
    a.set("key2", b"val", Some(Duration::ZERO)).await.unwrap();
    assert!(!fake.state.lock().unwrap().last_px.contains_key("key2"));
}

// ---------------------------------------------------------------------------
// Native verbs: SET NX, SCAN prefix deletion, DBSIZE stats, keys helper
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_if_absent_nx() {
    // pyfly put_if_absent over Redis SET NX.
    let (_fake, a) = adapter().await;
    assert!(a.set_if_absent("k", b"first", None).await.unwrap());
    assert!(!a.set_if_absent("k", b"second", None).await.unwrap());
    assert_eq!(a.get("k").await.unwrap(), b"first");
}

#[tokio::test]
async fn set_if_absent_with_ttl_forwards_px() {
    let fake = FakeRedis::start().await;
    let a = RedisAdapter::connect(&fake.url()).await.unwrap();
    assert!(a
        .set_if_absent("k", b"v", Some(Duration::from_secs(30)))
        .await
        .unwrap());
    assert_eq!(
        fake.state.lock().unwrap().last_px.get("k").copied(),
        Some(30_000)
    );
}

#[tokio::test]
async fn delete_prefix_scans_and_deletes() {
    // pyfly evict_by_prefix over SCAN MATCH + DEL.
    let (_fake, a) = adapter().await;
    a.set("user:1", b"a", None).await.unwrap();
    a.set("user:2", b"b", None).await.unwrap();
    a.set("order:1", b"c", None).await.unwrap();
    let removed = a.delete_prefix("user:").await.unwrap();
    assert_eq!(removed, 2);
    assert!(a.get("user:1").await.unwrap_err().is_not_found());
    assert_eq!(a.get("order:1").await.unwrap(), b"c");
}

#[tokio::test]
async fn delete_prefix_no_match_returns_zero() {
    let (_fake, a) = adapter().await;
    a.set("order:1", b"c", None).await.unwrap();
    assert_eq!(a.delete_prefix("user:").await.unwrap(), 0);
}

#[tokio::test]
async fn delete_prefix_escapes_glob_metachars() {
    // A literal prefix containing a glob char must not match unrelated keys.
    let (_fake, a) = adapter().await;
    a.set("a*b:1", b"x", None).await.unwrap();
    a.set("axxb:1", b"y", None).await.unwrap(); // would match if '*' were a wildcard
    let removed = a.delete_prefix("a*b:").await.unwrap();
    assert_eq!(removed, 1, "only the literal 'a*b:' key should be removed");
    assert_eq!(a.get("axxb:1").await.unwrap(), b"y");
}

#[tokio::test]
async fn stats_reports_dbsize_and_counters() {
    // pyfly get_stats: size from DBSIZE; hits/misses/hit_rate from counters.
    let (_fake, a) = adapter().await;
    a.set("a", b"1", None).await.unwrap();
    a.set("b", b"2", None).await.unwrap();
    a.get("a").await.unwrap(); // hit
    assert!(a.get("missing").await.unwrap_err().is_not_found()); // miss

    let stats = a.stats().await.unwrap();
    assert_eq!(stats.size, 2, "DBSIZE should report 2 keys");
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.hit_rate, 0.5);
}

#[tokio::test]
async fn stats_counts_evictions() {
    let (_fake, a) = adapter().await;
    a.set("a", b"1", None).await.unwrap();
    a.set("p:1", b"x", None).await.unwrap();
    a.set("p:2", b"y", None).await.unwrap();
    a.delete("a").await.unwrap();
    assert_eq!(a.delete_prefix("p:").await.unwrap(), 2);
    let stats = a.stats().await.unwrap();
    assert_eq!(stats.evictions, 3);
}

#[tokio::test]
async fn keys_helper_matches_and_limits() {
    // pyfly get_keys(pattern, limit) over SCAN MATCH.
    let (_fake, a) = adapter().await;
    for i in 0..5 {
        a.set(&format!("user:{i}"), b"v", None).await.unwrap();
    }
    a.set("order:1", b"v", None).await.unwrap();

    let mut keys = a.keys("user:*", 100).await.unwrap();
    keys.sort();
    assert_eq!(keys, vec!["user:0", "user:1", "user:2", "user:3", "user:4"]);

    // Limit caps the result count.
    let limited = a.keys("user:*", 2).await.unwrap();
    assert_eq!(limited.len(), 2);

    // A zero limit yields nothing.
    assert!(a.keys("*", 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn name_is_redis_and_health_ok() {
    let (_fake, a) = adapter().await;
    assert_eq!(a.name(), "redis");
    a.health_check().await.unwrap();
    assert!(a.is_available().await);
}

// ---------------------------------------------------------------------------
// Error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_to_unreachable_is_backend_error() {
    // Port 1 is reserved (tcpmux) and refuses connections instantly.
    let err = RedisAdapter::connect("redis://127.0.0.1:1/0")
        .await
        .unwrap_err();
    assert!(matches!(err, CacheError::Backend(_)), "got {err}");
}

#[tokio::test]
async fn malformed_url_is_backend_error() {
    let err = RedisAdapter::connect("not-a-redis-url").await.unwrap_err();
    assert!(matches!(err, CacheError::Backend(_)), "got {err}");
}

// ---------------------------------------------------------------------------
// Typed facade over the Redis adapter (JSON wire compatibility)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct User {
    id: String,
    name: String,
}

#[tokio::test]
async fn typed_round_trip_over_redis() {
    let fake = FakeRedis::start().await;
    let a = std::sync::Arc::new(RedisAdapter::connect(&fake.url()).await.unwrap());
    let typed: Typed<User> = Typed::new(a.clone());

    let u = User {
        id: "u1".into(),
        name: "alice".into(),
    };
    typed
        .set("u:1", &u, Some(Duration::from_secs(60)))
        .await
        .unwrap();
    assert_eq!(typed.get("u:1").await.unwrap(), u);

    // Stored bytes are the same Go/JSON-compatible form as MemoryAdapter.
    let raw = a.get("u:1").await.unwrap();
    assert_eq!(raw, br#"{"id":"u1","name":"alice"}"#);
}

#[tokio::test]
async fn typed_get_or_set_over_redis() {
    let fake = FakeRedis::start().await;
    let a = std::sync::Arc::new(RedisAdapter::connect(&fake.url()).await.unwrap());
    let typed: Typed<User> = Typed::new(a);

    let loaded = std::sync::atomic::AtomicUsize::new(0);
    for _ in 0..2 {
        let u = typed
            .get_or_set("u:2", None, || async {
                loaded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(User {
                    id: "u2".into(),
                    name: "bob".into(),
                })
            })
            .await
            .unwrap();
        assert_eq!(u.name, "bob");
    }
    assert_eq!(
        loaded.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "loader must run once"
    );
}

// ---------------------------------------------------------------------------
// Object safety / composability behind Arc<dyn Adapter>
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usable_as_dyn_adapter() {
    let fake = FakeRedis::start().await;
    let a: std::sync::Arc<dyn Adapter> =
        std::sync::Arc::new(RedisAdapter::connect(&fake.url()).await.unwrap());
    a.set("k", b"v", None).await.unwrap();
    assert_eq!(a.get("k").await.unwrap(), b"v");
    assert_eq!(a.name(), "redis");
}

// ---------------------------------------------------------------------------
// Docker-gated round-trip against a real Redis (mirrors pyfly's
// test_cache_redis_integration.py). Run with `--ignored` and a Redis at
// REDIS_URL (default redis://127.0.0.1:6379/0).
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a real Redis at REDIS_URL"]
async fn real_redis_round_trip() {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_owned());
    let a = RedisAdapter::connect(&url).await.unwrap();
    a.clear().await.unwrap();

    a.set("user:1", b"alice", None).await.unwrap();
    a.set("user:2", b"bob", None).await.unwrap();
    assert_eq!(a.get("user:1").await.unwrap(), b"alice");
    assert!(a.exists("user:1").await.unwrap());

    assert!(!a.set_if_absent("user:1", b"nope", None).await.unwrap());
    assert!(a.set_if_absent("user:3", b"carol", None).await.unwrap());

    assert_eq!(a.delete_prefix("user:").await.unwrap(), 3);
    assert!(!a.exists("user:1").await.unwrap());

    a.health_check().await.unwrap();
}
