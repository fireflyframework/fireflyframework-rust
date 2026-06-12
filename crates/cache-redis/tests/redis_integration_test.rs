//! Env-gated **live** integration tests for
//! [`firefly_cache_redis::RedisAdapter`] against a real Redis.
//!
//! These complement the in-process fake-RESP server contract tests in
//! `redis_adapter_test.rs` (which always run, with no external service)
//! by exercising the genuine Redis wire protocol end to end: `SET` with
//! `PX`/`NX`, `GET`, `EXISTS`, `SCAN`-driven `delete_prefix`, `DBSIZE`
//! stats, `PING` health, and a `FLUSHDB` clear on an isolated logical
//! database.
//!
//! ## How they gate
//!
//! Each test reads `FIREFLY_TEST_REDIS_URL` (falling back to the older
//! `REDIS_URL`). When **unset**, the test prints a one-line `skipping …`
//! notice and returns — so `cargo test` on a machine with no Redis is
//! green. When **set**, the test performs a real round-trip against the
//! service the URL points at.
//!
//! ## Isolation & cleanup
//!
//! Every key-scoped test namespaces its keys under a unique prefix derived
//! from the test function name plus a process-unique atomic counter (never
//! `rand`), so concurrent runs and repeated runs never collide, and each
//! deletes the keys it created.
//!
//! The two **whole-DB** tests — the one that calls `FLUSHDB` (`clear`) and the
//! one that asserts a deterministic `DBSIZE` — cannot be isolated by a key
//! prefix, because `FLUSHDB`/`DBSIZE` are database-wide. They each run against
//! a **dedicated, fixed logical database** (a distinct high index in the
//! standard 16-database `0..=15` range) so they never wipe — or get perturbed
//! by — the key-scoped tests (which all run on the base URL's default DB) or
//! each other. Fixed indices (not counter-derived) are used so the index can
//! never drift out of the valid `0..=15` range nor collide under parallelism.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use firefly_cache::{Adapter, CacheError};
use firefly_cache_redis::RedisAdapter;

/// The standard Firefly integration env var (preferred), with the older
/// `REDIS_URL` accepted as a fallback so tests that historically read it
/// keep working.
fn redis_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Process-unique, monotonically increasing counter. Combined with the
/// test function name it yields collision-free key prefixes without ever
/// touching a random source.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Dedicated logical DB for the deterministic-`DBSIZE` stats test. A fixed
/// high index in the standard `0..=15` range, distinct from
/// [`FLUSHDB_DB`] and from the key-scoped tests' default DB, so its
/// `DBSIZE` is never perturbed.
const STATS_DB: u64 = 14;

/// Dedicated logical DB for the `FLUSHDB` (`clear`) test. A fixed high
/// index in the standard `0..=15` range, distinct from [`STATS_DB`] and
/// from the key-scoped tests' default DB, so its `FLUSHDB` only wipes this
/// test's own keys.
const FLUSHDB_DB: u64 = 15;

/// A unique key prefix for `test`, e.g. `firefly:it:set_get_round_trip:3:`.
fn unique_prefix(test: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("firefly:it:{test}:{n}:")
}

/// Connects a [`RedisAdapter`] to the live Redis at `url`, or returns the
/// connection error for the caller to surface.
async fn connect(url: &str) -> Result<RedisAdapter, CacheError> {
    RedisAdapter::connect(url).await
}

/// Derives a per-test Redis URL pointing at a dedicated logical database
/// index so a `FLUSHDB` only clears this test's data. The base URL may or
/// may not already carry a `/<db>` path; we replace (or append) it.
fn url_with_db(base: &str, db: u64) -> String {
    // Strip a trailing `/<digits>` db segment if present, then append ours.
    let trimmed = base.trim_end_matches('/');
    let without_db = match trimmed.rsplit_once('/') {
        // `redis://host:port/0` -> base is everything before the last `/`
        // when the last segment is all digits.
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => trimmed,
    };
    format!("{without_db}/{db}")
}

// ---------------------------------------------------------------------------
// SET / GET / EXISTS / delete_prefix (SCAN) round-trips on an isolated prefix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_get_exists_round_trip() {
    let Some(url) = redis_url() else {
        eprintln!(
            "skipping set_get_exists_round_trip: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset"
        );
        return;
    };
    let a = connect(&url)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");
    let p = unique_prefix("set_get_exists_round_trip");

    let k1 = format!("{p}user:1");
    let k2 = format!("{p}user:2");

    // SET (no TTL) then GET round-trips the exact bytes.
    a.set(&k1, b"alice", None).await.unwrap();
    a.set(&k2, b"bob", None).await.unwrap();
    assert_eq!(a.get(&k1).await.unwrap(), b"alice");
    assert_eq!(a.get(&k2).await.unwrap(), b"bob");

    // EXISTS reflects presence / absence.
    assert!(a.exists(&k1).await.unwrap());
    assert!(!a.exists(&format!("{p}absent")).await.unwrap());

    // A missing key is a typed NotFound, not an error.
    assert!(a
        .get(&format!("{p}absent"))
        .await
        .unwrap_err()
        .is_not_found());

    // Cleanup: remove everything under this test's prefix.
    let removed = a.delete_prefix(&p).await.unwrap();
    assert_eq!(removed, 2, "delete_prefix should remove the two keys set");
    assert!(!a.exists(&k1).await.unwrap());
    assert!(!a.exists(&k2).await.unwrap());
}

#[tokio::test]
async fn set_px_ttl_expires() {
    let Some(url) = redis_url() else {
        eprintln!("skipping set_px_ttl_expires: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let a = connect(&url)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");
    let p = unique_prefix("set_px_ttl_expires");
    let k = format!("{p}ephemeral");

    // SET key value PX 80 — the value is readable immediately and gone
    // after the TTL elapses (Redis honours the PX expiry).
    a.set(&k, b"v", Some(Duration::from_millis(80)))
        .await
        .unwrap();
    assert_eq!(a.get(&k).await.unwrap(), b"v");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        a.get(&k).await.unwrap_err().is_not_found(),
        "key must expire after its PX TTL"
    );

    // Nothing to clean up (the key self-expired); be defensive anyway.
    let _ = a.delete_prefix(&p).await;
}

#[tokio::test]
async fn set_if_absent_nx_is_atomic() {
    let Some(url) = redis_url() else {
        eprintln!(
            "skipping set_if_absent_nx_is_atomic: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset"
        );
        return;
    };
    let a = connect(&url)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");
    let p = unique_prefix("set_if_absent_nx_is_atomic");
    let k = format!("{p}lock");

    // First NX write wins; the second is refused and the value is intact.
    assert!(a.set_if_absent(&k, b"first", None).await.unwrap());
    assert!(!a.set_if_absent(&k, b"second", None).await.unwrap());
    assert_eq!(a.get(&k).await.unwrap(), b"first");

    // NX with a PX TTL still sets when absent, and the key expires.
    let k_ttl = format!("{p}lock:ttl");
    assert!(a
        .set_if_absent(&k_ttl, b"v", Some(Duration::from_millis(80)))
        .await
        .unwrap());
    assert_eq!(a.get(&k_ttl).await.unwrap(), b"v");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(a.get(&k_ttl).await.unwrap_err().is_not_found());

    // Cleanup.
    let _ = a.delete_prefix(&p).await;
}

#[tokio::test]
async fn delete_prefix_scans_only_matching_keys() {
    let Some(url) = redis_url() else {
        eprintln!("skipping delete_prefix_scans_only_matching_keys: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let a = connect(&url)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");
    let p = unique_prefix("delete_prefix_scans_only_matching_keys");

    // Two namespaces under the same test prefix; deleting one must leave
    // the other untouched (SCAN MATCH <prefix>user:* + DEL).
    for i in 0..5 {
        a.set(&format!("{p}user:{i}"), b"v", None).await.unwrap();
    }
    a.set(&format!("{p}order:1"), b"keep", None).await.unwrap();

    let removed = a.delete_prefix(&format!("{p}user:")).await.unwrap();
    assert_eq!(removed, 5, "all five user keys removed");
    assert!(a
        .get(&format!("{p}user:0"))
        .await
        .unwrap_err()
        .is_not_found());
    assert_eq!(a.get(&format!("{p}order:1")).await.unwrap(), b"keep");

    // No-match prefix removes nothing.
    assert_eq!(a.delete_prefix(&format!("{p}nope:")).await.unwrap(), 0);

    // Cleanup the surviving key.
    let _ = a.delete_prefix(&p).await;
}

#[tokio::test]
async fn keys_helper_matches_and_limits() {
    let Some(url) = redis_url() else {
        eprintln!("skipping keys_helper_matches_and_limits: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let a = connect(&url)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");
    let p = unique_prefix("keys_helper_matches_and_limits");

    for i in 0..4 {
        a.set(&format!("{p}user:{i}"), b"v", None).await.unwrap();
    }
    a.set(&format!("{p}order:1"), b"v", None).await.unwrap();

    let mut matched = a.keys(&format!("{p}user:*"), 100).await.unwrap();
    matched.sort();
    let expected: Vec<String> = (0..4).map(|i| format!("{p}user:{i}")).collect();
    assert_eq!(matched, expected);

    // A limit caps the returned set.
    assert!(a.keys(&format!("{p}user:*"), 2).await.unwrap().len() <= 2);
    // A zero limit yields nothing.
    assert!(a.keys(&format!("{p}*"), 0).await.unwrap().is_empty());

    let _ = a.delete_prefix(&p).await;
}

// ---------------------------------------------------------------------------
// DBSIZE-backed stats + PING health (on an isolated logical DB so DBSIZE is
// deterministic and not perturbed by sibling tests / other clients)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stats_report_dbsize_and_hit_rate() {
    let Some(url) = redis_url() else {
        eprintln!("skipping stats_report_dbsize_and_hit_rate: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    // Use a dedicated logical DB so DBSIZE reflects only this test.
    let scoped = url_with_db(&url, STATS_DB);
    let a = connect(&scoped)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");

    // Start from a clean isolated DB.
    a.clear().await.unwrap();

    let p = unique_prefix("stats_report_dbsize_and_hit_rate");
    a.set(&format!("{p}a"), b"1", None).await.unwrap();
    a.set(&format!("{p}b"), b"2", None).await.unwrap();
    a.get(&format!("{p}a")).await.unwrap(); // hit
    assert!(a
        .get(&format!("{p}missing"))
        .await
        .unwrap_err()
        .is_not_found()); // miss

    let stats = a.stats().await.expect("stats");
    assert_eq!(stats.size, 2, "DBSIZE should report exactly the two keys");
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert!((stats.hit_rate - 0.5).abs() < 1e-9);

    // Cleanup the isolated DB entirely.
    a.clear().await.unwrap();
}

#[tokio::test]
async fn clear_flushdb_on_isolated_db() {
    let Some(url) = redis_url() else {
        eprintln!(
            "skipping clear_flushdb_on_isolated_db: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset"
        );
        return;
    };
    // FLUSHDB is database-wide, so target a dedicated, fixed logical DB to
    // avoid wiping any other test's keys (and to stay within the valid
    // `0..=15` range).
    let scoped = url_with_db(&url, FLUSHDB_DB);
    let a = connect(&scoped)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");

    // Start from a clean isolated DB so a residual key from a previous run
    // (or a parallel invocation of this same test) cannot skew the counts.
    a.clear().await.unwrap();

    a.set("a", b"1", None).await.unwrap();
    a.set("b", b"2", None).await.unwrap();
    assert!(a.exists("a").await.unwrap());

    // FLUSHDB clears this isolated DB.
    a.clear().await.unwrap();
    assert!(a.get("a").await.unwrap_err().is_not_found());
    assert!(a.get("b").await.unwrap_err().is_not_found());
    assert_eq!(a.stats().await.expect("stats").size, 0);
}

#[tokio::test]
async fn health_check_and_availability() {
    let Some(url) = redis_url() else {
        eprintln!("skipping health_check_and_availability: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let a = connect(&url)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");
    assert_eq!(a.name(), "redis");
    a.health_check().await.unwrap();
    assert!(a.is_available().await);
}
