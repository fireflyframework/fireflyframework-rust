//! Live round-trip integration tests for
//! [`firefly_cache_postgres::PostgresCacheAdapter`], ported from pyfly's
//! `tests/cache/test_postgres_cache_adapter.py`.
//!
//! These are **env-gated** integration tests, not `#[ignore]`-gated. Each
//! test reads `FIREFLY_TEST_POSTGRES_URL` (falling back to the older
//! `DATABASE_URL` / `POSTGRES_URL`); when the variable is **unset** it prints
//! a one-line `skipping …` and returns, so `cargo test` on a bare machine is
//! green. When the variable is **set** it performs the genuine round-trip
//! against a real Postgres: create the table, set / get / delete / stats, and
//! cleans up after itself.
//!
//! ## Parallel isolation
//!
//! Every test gets its **own uniquely-named table** via
//! [`PostgresCacheAdapter::connect_with_table`] (named
//! `fftest_cache_<slug>_<pid>_<n>`), `init()`s it, and `DROP`s it at the end.
//! Because no two tests share a table, assertions on table-wide state
//! (`stats` `COUNT(*)`, `keys`, miss/hit semantics) are unaffected by other
//! tests' rows — so the suite is correct under the default parallel runner,
//! with no `--test-threads=1` required.
//!
//! Run against a live database with:
//!
//! ```sh
//! export FIREFLY_TEST_POSTGRES_URL="postgres://firefly:firefly@localhost:5432/firefly"
//! cargo test -p firefly-cache-postgres
//! ```
//!
//! Everything that *can* be verified without a live DB (the SQL/DDL strings,
//! table-name validation, the glob→LIKE / TTL / DSN logic, and `Adapter`
//! object-safety) is covered by the unit tests inside the crate (`src/lib.rs`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use firefly_cache::{Adapter, Typed};
use firefly_cache_postgres::PostgresCacheAdapter;

/// Process-wide monotonic counter, combined with the per-test slug and the
/// process id, so every test gets a unique table name even when the suite runs
/// in parallel against a shared database — derived deterministically, not from
/// a random source.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the integration database URL from the standard env var (with the
/// older fallbacks). Returns `None` when none is set so callers can early-skip.
fn pg_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_POSTGRES_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .or_else(|_| std::env::var("POSTGRES_URL"))
        .ok()
}

/// Builds a collision-free table name for `slug`, unique per process + call,
/// sanitised to the validator's `[a-z0-9_]` alphabet so
/// [`PostgresCacheAdapter::connect_with_table`] always accepts it.
fn unique_table(slug: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let sanitised: String = slug
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("fftest_cache_{sanitised}_{}_{n}", std::process::id())
}

/// RAII handle that `DROP`s a per-test table when the test ends — even on
/// panic — so a test leaves no residue in the shared database. Independent of
/// the adapter so it works whether the adapter is held by value or behind an
/// `Arc<dyn Adapter>`.
struct TableGuard {
    url: String,
    table: String,
}

impl Drop for TableGuard {
    fn drop(&mut self) {
        // Best-effort cleanup on a throwaway connection: spin up a tiny runtime
        // on a fresh thread (we may be dropping inside the test's own async
        // runtime, so we cannot block on it here) and DROP the per-test table.
        let url = self.url.clone();
        let table = self.table.clone();
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                if let Ok((client, connection)) =
                    tokio_postgres::connect(&url, tokio_postgres::NoTls).await
                {
                    let handle = tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    // `table` is the validator-approved name we created with,
                    // so this interpolation is safe.
                    let _ = client
                        .batch_execute(&format!("DROP TABLE IF EXISTS {table}"))
                        .await;
                    drop(client);
                    handle.abort();
                }
            });
        })
        .join();
    }
}

/// An adapter bound to its own freshly-created table; the table is dropped when
/// the bundled [`TableGuard`] goes out of scope.
struct TestCache {
    adapter: PostgresCacheAdapter,
    _guard: TableGuard,
}

impl std::ops::Deref for TestCache {
    type Target = PostgresCacheAdapter;
    fn deref(&self) -> &Self::Target {
        &self.adapter
    }
}

/// Connects + initialises a fresh adapter against the integration database on
/// its own uniquely-named table (created here, dropped when the returned
/// handle is dropped).
async fn adapter(url: &str, table: &str) -> TestCache {
    let a = PostgresCacheAdapter::connect_with_table(url, table)
        .await
        .expect("connect to FIREFLY_TEST_POSTGRES_URL");
    a.init().await.expect("init creates the per-test table");
    TestCache {
        adapter: a,
        _guard: TableGuard {
            url: url.to_owned(),
            table: table.to_owned(),
        },
    }
}

#[tokio::test]
async fn put_and_get_scalar() {
    let Some(url) = pg_url() else {
        eprintln!("skipping put_and_get_scalar: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("put")).await;
    a.set("num", b"123", None).await.unwrap();
    assert_eq!(a.get("num").await.unwrap(), b"123");
}

#[tokio::test]
async fn get_missing_returns_not_found() {
    let Some(url) = pg_url() else {
        eprintln!("skipping get_missing_returns_not_found: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("miss")).await;
    let err = a.get("no-such-key").await.unwrap_err();
    assert!(err.is_not_found(), "got {err}");
}

#[tokio::test]
async fn put_overwrites() {
    let Some(url) = pg_url() else {
        eprintln!("skipping put_overwrites: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("ow")).await;
    a.set("k", b"first", None).await.unwrap();
    a.set("k", b"second", None).await.unwrap();
    assert_eq!(a.get("k").await.unwrap(), b"second");
}

#[tokio::test]
async fn exists_true_and_false() {
    let Some(url) = pg_url() else {
        eprintln!("skipping exists_true_and_false: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("ex")).await;
    a.set("e", b"v", None).await.unwrap();
    assert!(a.exists("e").await.unwrap());
    assert!(!a.exists("missing").await.unwrap());
}

#[tokio::test]
async fn delete_then_get_is_miss() {
    let Some(url) = pg_url() else {
        eprintln!("skipping delete_then_get_is_miss: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("del")).await;
    a.set("k", b"v", None).await.unwrap();
    a.delete("k").await.unwrap();
    assert!(a.get("k").await.unwrap_err().is_not_found());
    // Deleting a missing key is a no-op.
    a.delete("no-such").await.unwrap();
}

#[tokio::test]
async fn delete_prefix_removes_matching_only() {
    let Some(url) = pg_url() else {
        eprintln!(
            "skipping delete_prefix_removes_matching_only: set FIREFLY_TEST_POSTGRES_URL to run"
        );
        return;
    };
    let a = adapter(&url, &unique_table("dp")).await;
    a.set("p:1", b"1", None).await.unwrap();
    a.set("p:2", b"2", None).await.unwrap();
    a.set("q:3", b"3", None).await.unwrap();
    let count = a.delete_prefix("p:").await.unwrap();
    assert_eq!(count, 2);
    assert!(a.get("p:1").await.unwrap_err().is_not_found());
    assert!(a.get("p:2").await.unwrap_err().is_not_found());
    assert_eq!(a.get("q:3").await.unwrap(), b"3");
}

#[tokio::test]
async fn set_if_absent_semantics() {
    let Some(url) = pg_url() else {
        eprintln!("skipping set_if_absent_semantics: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("sia")).await;
    assert!(a.set_if_absent("fresh", b"v", None).await.unwrap());
    assert_eq!(a.get("fresh").await.unwrap(), b"v");

    a.set("exists", b"original", None).await.unwrap();
    assert!(!a.set_if_absent("exists", b"other", None).await.unwrap());
    assert_eq!(a.get("exists").await.unwrap(), b"original");
}

#[tokio::test]
async fn ttl_expires_at_read_time() {
    let Some(url) = pg_url() else {
        eprintln!("skipping ttl_expires_at_read_time: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("ttl")).await;
    a.set("k", b"v", Some(Duration::from_millis(50)))
        .await
        .unwrap();
    assert_eq!(a.get("k").await.unwrap(), b"v");
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(a.get("k").await.unwrap_err().is_not_found());
}

#[tokio::test]
async fn keys_match_pattern_and_limit() {
    let Some(url) = pg_url() else {
        eprintln!("skipping keys_match_pattern_and_limit: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("keys")).await;
    a.set("ns:a", b"1", None).await.unwrap();
    a.set("ns:b", b"2", None).await.unwrap();
    a.set("other:c", b"3", None).await.unwrap();
    let mut matched = a.keys("ns:*", 100).await.unwrap();
    matched.sort();
    assert_eq!(matched, vec!["ns:a".to_owned(), "ns:b".to_owned()]);
    // limit caps the result set (3 keys exist, ask for 1).
    assert!(a.keys("*", 1).await.unwrap().len() <= 1);
    // limit 0 returns nothing.
    assert!(a.keys("*", 0).await.unwrap().is_empty());
    // A full-table glob sees exactly the three keys this test wrote — no other
    // test can leak in, because the table is private to this test.
    let mut all = a.keys("*", 100).await.unwrap();
    all.sort();
    assert_eq!(
        all,
        vec!["ns:a".to_owned(), "ns:b".to_owned(), "other:c".to_owned()]
    );
}

#[tokio::test]
async fn stats_track_hits_misses_and_size() {
    let Some(url) = pg_url() else {
        eprintln!(
            "skipping stats_track_hits_misses_and_size: set FIREFLY_TEST_POSTGRES_URL to run"
        );
        return;
    };
    let a = adapter(&url, &unique_table("stat")).await;
    a.set("k", b"v", None).await.unwrap();
    let _ = a.get("k").await; // hit
    let _ = a.get("missing").await; // miss
    let stats = a.stats().await.expect("stats");
    // Hit/miss counters are per-adapter in-process counters: this adapter
    // recorded exactly one hit and one miss.
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert!((stats.hit_rate - 0.5).abs() < 1e-9);
    // size is a table-wide COUNT, but the table is private to this test, so it
    // is exactly the one row we wrote — no parallel leakage.
    assert_eq!(stats.size, 1);
}

#[tokio::test]
async fn name_and_health_check() {
    let Some(url) = pg_url() else {
        eprintln!("skipping name_and_health_check: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let a = adapter(&url, &unique_table("meta")).await;
    assert_eq!(a.name(), "postgres");
    a.health_check().await.unwrap();
    assert!(a.is_available().await);
}

#[tokio::test]
async fn typed_facade_round_trip() {
    let Some(url) = pg_url() else {
        eprintln!("skipping typed_facade_round_trip: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Order {
        id: String,
    }
    // `Typed::new` needs an `Arc<dyn Adapter>`, so build the adapter into an Arc
    // directly and keep a TableGuard alongside to drop the per-test table.
    let table = unique_table("typed");
    let a = Arc::new(
        PostgresCacheAdapter::connect_with_table(&url, &table)
            .await
            .expect("connect to FIREFLY_TEST_POSTGRES_URL"),
    );
    a.init().await.expect("init creates the per-test table");
    let _guard = TableGuard {
        url: url.clone(),
        table,
    };
    let erased: Arc<dyn Adapter> = Arc::clone(&a) as Arc<dyn Adapter>;
    let typed: Typed<Order> = Typed::new(erased);
    let got = typed
        .get_or_set("order:42", Some(Duration::from_secs(60)), || async {
            Ok(Order { id: "42".into() })
        })
        .await
        .unwrap();
    assert_eq!(got, Order { id: "42".into() });
}
