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

//! Live round-trip integration tests for
//! [`firefly_session_postgres::PostgresSessionRegistry`].
//!
//! These are **env-gated** integration tests. Each test reads
//! `FIREFLY_TEST_POSTGRES_URL` (falling back to the older `DATABASE_URL` /
//! `POSTGRES_URL`); when the variable is **unset** it prints a one-line
//! `skipping …` and returns, so `cargo test` on a bare machine is green. When
//! the variable is **set** it performs the genuine round-trip against a real
//! Postgres: auto-create the table, register / list / count / deregister, and
//! cleans up after itself.
//!
//! ## Parallel isolation
//!
//! Every test gets its **own uniquely-named table** via
//! [`PostgresSessionRegistry::connect_with_table`] (named
//! `fftest_sess_<slug>_<pid>_<n>`), exercises the **lazy auto-DDL** (the table
//! is created on first use, not eagerly), and `DROP`s the table at the end via
//! an RAII guard. Because no two tests share a table, per-principal assertions
//! are unaffected by other tests' rows — the suite is correct under the default
//! parallel runner.
//!
//! Everything that *can* be verified without a live DB (the SQL/DDL strings,
//! table-name validation, the DSN logic, and `SessionRegistry` object-safety)
//! is covered by the unit tests inside the crate (`src/lib.rs`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use firefly_session::{ConcurrencyPolicy, SessionConcurrencyController, SessionRegistry, Strategy};
use firefly_session_postgres::PostgresSessionRegistry;

/// Process-wide monotonic counter, combined with the per-test slug and the
/// process id, so every test gets a unique table name even under the parallel
/// runner — derived deterministically, not from a random source.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the integration database URL from the standard env var (with the older
/// fallbacks). Returns `None` when none is set so callers can early-skip.
fn pg_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_POSTGRES_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .or_else(|_| std::env::var("POSTGRES_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Builds a collision-free table name for `slug`, unique per process + call,
/// sanitised to the validator's `[a-z0-9_]` alphabet.
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
    format!("fftest_sess_{sanitised}_{}_{n}", std::process::id())
}

/// RAII handle that `DROP`s a per-test table when the test ends — even on
/// panic — so a test leaves no residue.
struct TableGuard {
    url: String,
    table: String,
}

impl Drop for TableGuard {
    fn drop(&mut self) {
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
                    // `table` is the validator-approved name we created with, so
                    // this interpolation is safe.
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

/// A registry bound to its own table; the table is dropped when the bundled
/// [`TableGuard`] goes out of scope.
struct TestRegistry {
    registry: PostgresSessionRegistry,
    _guard: TableGuard,
}

impl std::ops::Deref for TestRegistry {
    type Target = PostgresSessionRegistry;
    fn deref(&self) -> &Self::Target {
        &self.registry
    }
}

/// Connects a fresh registry against the integration database on its own
/// uniquely-named table (dropped when the returned handle is dropped). Does NOT
/// call `init()`, so the lazy auto-DDL is exercised on first use.
async fn registry(url: &str, table: &str) -> TestRegistry {
    let r = PostgresSessionRegistry::connect_with_table(url, table)
        .await
        .expect("connect to FIREFLY_TEST_POSTGRES_URL");
    TestRegistry {
        registry: r,
        _guard: TableGuard {
            url: url.to_owned(),
            table: table.to_owned(),
        },
    }
}

#[tokio::test]
async fn auto_ddl_register_list_count_deregister() {
    let Some(url) = pg_url() else {
        eprintln!("skipping auto_ddl_register_list_count_deregister: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    // No init() — the first register() must lazily create the table.
    let r = registry(&url, &unique_table("flow")).await;
    let p = "alice";

    // Register out of order; list_sessions comes back oldest-first.
    r.register(p, "s2", 2000).await;
    r.register(p, "s1", 1000).await;
    r.register(p, "s3", 3000).await;

    assert_eq!(r.count(p).await, 3);
    assert_eq!(
        r.list_sessions(p).await,
        vec![
            ("s1".to_owned(), 1000),
            ("s2".to_owned(), 2000),
            ("s3".to_owned(), 3000),
        ]
    );

    // Deregister the middle session.
    r.deregister(p, "s2").await;
    assert_eq!(r.count(p).await, 2);
    assert_eq!(
        r.list_sessions(p).await,
        vec![("s1".to_owned(), 1000), ("s3".to_owned(), 3000)]
    );

    // Deregistering a missing session is an idempotent no-op.
    r.deregister(p, "s2").await;
    assert_eq!(r.count(p).await, 2);
}

#[tokio::test]
async fn register_upserts_existing_session() {
    let Some(url) = pg_url() else {
        eprintln!(
            "skipping register_upserts_existing_session: set FIREFLY_TEST_POSTGRES_URL to run"
        );
        return;
    };
    let r = registry(&url, &unique_table("upsert")).await;
    r.register("bob", "s1", 1000).await;
    // Re-registering the same session id updates created_at rather than
    // inserting a duplicate (ON CONFLICT DO UPDATE).
    r.register("bob", "s1", 9000).await;
    assert_eq!(r.count("bob").await, 1);
    assert_eq!(r.list_sessions("bob").await, vec![("s1".to_owned(), 9000)]);
}

#[tokio::test]
async fn principals_are_isolated() {
    let Some(url) = pg_url() else {
        eprintln!("skipping principals_are_isolated: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let r = registry(&url, &unique_table("iso")).await;
    r.register("carol", "c1", 1).await;
    r.register("dave", "d1", 1).await;
    r.register("dave", "d2", 2).await;
    assert_eq!(r.count("carol").await, 1);
    assert_eq!(r.count("dave").await, 2);
    assert_eq!(r.count("nobody").await, 0);
    assert!(r.list_sessions("nobody").await.is_empty());
}

#[tokio::test]
async fn init_creates_table_eagerly() {
    let Some(url) = pg_url() else {
        eprintln!("skipping init_creates_table_eagerly: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let r = registry(&url, &unique_table("init")).await;
    // Eager DDL: init() must succeed and be idempotent.
    r.init().await.expect("init creates the table");
    r.init().await.expect("init is idempotent");
    r.register("erin", "s1", 1).await;
    assert_eq!(r.count("erin").await, 1);
}

#[tokio::test]
async fn is_available_health_check() {
    let Some(url) = pg_url() else {
        eprintln!("skipping is_available_health_check: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let r = registry(&url, &unique_table("health")).await;
    assert!(r.is_available().await);
}

/// The durable registry must satisfy the same concurrency-controller contract
/// the in-process registry does — evict-oldest holds the cap by dropping the
/// oldest (lowest created_at) session.
#[tokio::test]
async fn evict_oldest_through_controller() {
    let Some(url) = pg_url() else {
        eprintln!("skipping evict_oldest_through_controller: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };
    let table = unique_table("evict");
    let r = Arc::new(
        PostgresSessionRegistry::connect_with_table(&url, &table)
            .await
            .expect("connect to FIREFLY_TEST_POSTGRES_URL"),
    );
    let _guard = TableGuard {
        url: url.clone(),
        table,
    };
    let ctl = SessionConcurrencyController::new(
        r.clone(),
        ConcurrencyPolicy {
            max_sessions: 2,
            strategy: Strategy::EvictOldest,
        },
    );

    assert!(ctl.on_login("grace", "s1", 1).await);
    assert!(ctl.on_login("grace", "s2", 2).await);
    assert!(ctl.on_login("grace", "s3", 3).await); // evicts oldest (s1)

    assert_eq!(r.count("grace").await, 2);
    let ids: Vec<String> = r
        .list_sessions("grace")
        .await
        .into_iter()
        .map(|(sid, _)| sid)
        .collect();
    assert_eq!(ids, vec!["s2".to_owned(), "s3".to_owned()]);
}
