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

//! Env-gated **live** integration tests for
//! [`firefly_session_redis::RedisSessionRegistry`] against a real Redis.
//!
//! These complement the always-on in-process fake-RESP contract tests in
//! `redis_registry_test.rs` by exercising the genuine Redis wire protocol end
//! to end against a running server.
//!
//! ## How they gate
//!
//! Each test reads `FIREFLY_TEST_REDIS_URL` (falling back to the older
//! `REDIS_URL`). When **unset**, the test prints a one-line `skipping …`
//! notice and returns — so `cargo test` on a machine with no Redis is green.
//! When **set**, the test performs a real round-trip against the service the
//! URL points at.
//!
//! ## Isolation & cleanup
//!
//! Every test namespaces its principals under a unique key prefix derived from
//! the test function name plus a process-unique atomic counter (never `rand`),
//! so concurrent and repeated runs never collide, and each test deregisters
//! every session it created so it leaves no residue.

use std::sync::atomic::{AtomicU64, Ordering};

use firefly_session::SessionRegistry;
use firefly_session_redis::RedisSessionRegistry;

/// The standard Firefly integration env var (preferred), with the older
/// `REDIS_URL` accepted as a fallback.
fn redis_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Process-unique, monotonically increasing counter. Combined with the test
/// function name it yields collision-free key prefixes without ever touching a
/// random source.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique key prefix for `test`, e.g.
/// `firefly:it:session:register_and_list:3:user:`.
fn unique_prefix(test: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("firefly:it:session:{test}:{n}:user:")
}

#[tokio::test]
async fn register_list_count_deregister_round_trip() {
    let Some(url) = redis_url() else {
        eprintln!("skipping register_list_count_deregister_round_trip: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let prefix = unique_prefix("register_and_list");
    let reg = RedisSessionRegistry::connect_with(&url, &prefix, 3600)
        .await
        .expect("connect to FIREFLY_TEST_REDIS_URL");

    let p = "alice";

    // Register out of order; list_sessions must come back oldest-first.
    reg.register(p, "s2", 2000).await;
    reg.register(p, "s1", 1000).await;
    reg.register(p, "s3", 3000).await;

    assert_eq!(reg.count(p).await, 3);
    assert_eq!(
        reg.list_sessions(p).await,
        vec![
            ("s1".to_owned(), 1000),
            ("s2".to_owned(), 2000),
            ("s3".to_owned(), 3000),
        ]
    );

    // Deregister the middle one; the rest stay ordered.
    reg.deregister(p, "s2").await;
    assert_eq!(reg.count(p).await, 2);
    assert_eq!(
        reg.list_sessions(p).await,
        vec![("s1".to_owned(), 1000), ("s3".to_owned(), 3000)]
    );

    // Cleanup: deregister the remainder; the key self-prunes when empty.
    reg.deregister(p, "s1").await;
    reg.deregister(p, "s3").await;
    assert_eq!(reg.count(p).await, 0);
    assert!(reg.list_sessions(p).await.is_empty());
}

#[tokio::test]
async fn deregister_is_idempotent() {
    let Some(url) = redis_url() else {
        eprintln!(
            "skipping deregister_is_idempotent: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset"
        );
        return;
    };
    let prefix = unique_prefix("idempotent");
    let reg = RedisSessionRegistry::connect_with(&url, &prefix, 3600)
        .await
        .expect("connect");

    reg.register("bob", "s1", 1).await;
    reg.deregister("bob", "s1").await;
    // Deregistering again is a clean no-op.
    reg.deregister("bob", "s1").await;
    reg.deregister("bob", "never-existed").await;
    assert_eq!(reg.count("bob").await, 0);
}

#[tokio::test]
async fn health_check_pings() {
    let Some(url) = redis_url() else {
        eprintln!("skipping health_check_pings: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let reg = RedisSessionRegistry::connect(&url).await.expect("connect");
    assert!(reg.is_available().await);
}
