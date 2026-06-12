//! Round-trip behaviour-contract tests for
//! [`firefly_cache_postgres::PostgresCacheAdapter`], ported from pyfly's
//! `tests/cache/test_postgres_cache_adapter.py`.
//!
//! pyfly runs its adapter tests against an in-memory SQLite engine (via
//! `aiosqlite`) so no Docker is needed. `tokio-postgres` speaks the Postgres
//! binary wire protocol, which an in-process fake cannot faithfully emulate
//! for prepared statements + `BYTEA` + `TIMESTAMPTZ`, so the behavioural
//! round-trips here are **`#[ignore]`-gated** and require a real Postgres.
//!
//! Point them at a database with:
//!
//! ```sh
//! export FIREFLY_TEST_PG="postgresql://postgres:postgres@localhost:5432/postgres"
//! cargo test -p firefly-cache-postgres -- --ignored
//! ```
//!
//! Everything that *can* be verified without a live DB (the SQL/DDL strings,
//! the glob→LIKE / TTL / DSN logic, and `Adapter` object-safety) is covered
//! by the unit tests inside the crate (`src/lib.rs`).

use std::sync::Arc;
use std::time::Duration;

use firefly_cache::{Adapter, Typed};
use firefly_cache_postgres::PostgresCacheAdapter;

/// Connects + initialises a fresh adapter against `FIREFLY_TEST_PG`, using a
/// unique key prefix per test so concurrent runs against a shared database do
/// not collide. Returns the adapter and a prefix the test should namespace
/// its keys with.
async fn adapter(prefix: &str) -> PostgresCacheAdapter {
    let dsn = std::env::var("FIREFLY_TEST_PG")
        .expect("set FIREFLY_TEST_PG to a postgresql:// URL to run the ignored tests");
    let a = PostgresCacheAdapter::connect(&dsn)
        .await
        .expect("connect to FIREFLY_TEST_PG");
    a.init().await.expect("init creates the table");
    // Best-effort isolation: drop anything from a previous run.
    let _ = a.delete_prefix(prefix).await;
    a
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn put_and_get_scalar() {
    // pyfly test_put_and_get_scalar / test_put_and_get_dict (bytes here;
    // JSON handled by Typed below).
    let a = adapter("pg-test:put:").await;
    a.set("pg-test:put:num", b"123", None).await.unwrap();
    assert_eq!(a.get("pg-test:put:num").await.unwrap(), b"123");
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn get_missing_returns_not_found() {
    // pyfly test_get_missing_returns_none (None ⇒ NotFound at the byte port).
    let a = adapter("pg-test:miss:").await;
    let err = a.get("pg-test:miss:no-such-key").await.unwrap_err();
    assert!(err.is_not_found(), "got {err}");
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn put_overwrites() {
    // pyfly test_put_overwrites.
    let a = adapter("pg-test:ow:").await;
    a.set("pg-test:ow:k", b"first", None).await.unwrap();
    a.set("pg-test:ow:k", b"second", None).await.unwrap();
    assert_eq!(a.get("pg-test:ow:k").await.unwrap(), b"second");
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn exists_true_and_false() {
    // pyfly test_exists_true / test_exists_false.
    let a = adapter("pg-test:ex:").await;
    a.set("pg-test:ex:e", b"v", None).await.unwrap();
    assert!(a.exists("pg-test:ex:e").await.unwrap());
    assert!(!a.exists("pg-test:ex:missing").await.unwrap());
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn delete_then_get_is_miss() {
    // pyfly test_evict_returns_true / test_evict_missing_returns_false.
    let a = adapter("pg-test:del:").await;
    a.set("pg-test:del:k", b"v", None).await.unwrap();
    a.delete("pg-test:del:k").await.unwrap();
    assert!(a.get("pg-test:del:k").await.unwrap_err().is_not_found());
    // Deleting a missing key is a no-op.
    a.delete("pg-test:del:no-such").await.unwrap();
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn delete_prefix_removes_matching_only() {
    // pyfly test_evict_by_prefix.
    let a = adapter("pg-test:dp:").await;
    a.set("pg-test:dp:p:1", b"1", None).await.unwrap();
    a.set("pg-test:dp:p:2", b"2", None).await.unwrap();
    a.set("pg-test:dp:q:3", b"3", None).await.unwrap();
    let count = a.delete_prefix("pg-test:dp:p:").await.unwrap();
    assert_eq!(count, 2);
    assert!(a.get("pg-test:dp:p:1").await.unwrap_err().is_not_found());
    assert!(a.get("pg-test:dp:p:2").await.unwrap_err().is_not_found());
    assert_eq!(a.get("pg-test:dp:q:3").await.unwrap(), b"3");
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn set_if_absent_semantics() {
    // pyfly test_put_if_absent_returns_true_on_new_key /
    // test_put_if_absent_returns_false_on_existing_key.
    let a = adapter("pg-test:sia:").await;
    assert!(a
        .set_if_absent("pg-test:sia:fresh", b"v", None)
        .await
        .unwrap());
    assert_eq!(a.get("pg-test:sia:fresh").await.unwrap(), b"v");

    a.set("pg-test:sia:exists", b"original", None)
        .await
        .unwrap();
    assert!(!a
        .set_if_absent("pg-test:sia:exists", b"other", None)
        .await
        .unwrap());
    assert_eq!(a.get("pg-test:sia:exists").await.unwrap(), b"original");
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn ttl_expires_at_read_time() {
    // The expiry predicate hides a row whose expires_at is in the past.
    let a = adapter("pg-test:ttl:").await;
    a.set("pg-test:ttl:k", b"v", Some(Duration::from_millis(50)))
        .await
        .unwrap();
    assert_eq!(a.get("pg-test:ttl:k").await.unwrap(), b"v");
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(a.get("pg-test:ttl:k").await.unwrap_err().is_not_found());
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn clear_removes_all_visible_entries() {
    // pyfly test_clear_removes_all.
    let a = adapter("pg-test:clr:").await;
    a.set("pg-test:clr:a", b"1", None).await.unwrap();
    a.set("pg-test:clr:b", b"2", None).await.unwrap();
    a.clear().await.unwrap();
    assert!(a.get("pg-test:clr:a").await.unwrap_err().is_not_found());
    assert!(a.get("pg-test:clr:b").await.unwrap_err().is_not_found());
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn keys_match_pattern_and_limit() {
    // pyfly test_get_keys_all / test_get_keys_pattern / test_get_keys_limit.
    let a = adapter("pg-test:keys:").await;
    a.set("pg-test:keys:ns:a", b"1", None).await.unwrap();
    a.set("pg-test:keys:ns:b", b"2", None).await.unwrap();
    a.set("pg-test:keys:other:c", b"3", None).await.unwrap();
    let mut matched = a.keys("pg-test:keys:ns:*", 100).await.unwrap();
    matched.sort();
    assert_eq!(matched, vec!["pg-test:keys:ns:a", "pg-test:keys:ns:b"]);
    // limit caps the result set.
    assert!(a.keys("pg-test:keys:*", 1).await.unwrap().len() <= 1);
    // limit 0 returns nothing.
    assert!(a.keys("pg-test:keys:*", 0).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn stats_track_hits_misses_and_size() {
    // pyfly test_get_stats_hit_rate.
    let a = adapter("pg-test:stat:").await;
    a.set("pg-test:stat:k", b"v", None).await.unwrap();
    let _ = a.get("pg-test:stat:k").await; // hit
    let _ = a.get("pg-test:stat:missing").await; // miss
    let stats = a.stats().await.expect("stats");
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert!((stats.hit_rate - 0.5).abs() < 1e-9);
    assert!(stats.size >= 1);
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn name_and_health_check() {
    let a = adapter("pg-test:meta:").await;
    assert_eq!(a.name(), "postgres");
    a.health_check().await.unwrap();
    assert!(a.is_available().await);
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn typed_facade_round_trip() {
    // The byte port composes with the JSON Typed<T> facade just like the
    // memory/redis adapters (pyfly test_put_and_get_dict at the object level).
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Order {
        id: String,
    }
    let a = Arc::new(adapter("pg-test:typed:").await);
    let typed: Typed<Order> = Typed::new(a);
    let got = typed
        .get_or_set(
            "pg-test:typed:order:42",
            Some(Duration::from_secs(60)),
            || async { Ok(Order { id: "42".into() }) },
        )
        .await
        .unwrap();
    assert_eq!(got, Order { id: "42".into() });
}
