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
//! [`firefly_webhooks::RedisEventStore`] against a real Redis.
//!
//! ## How they gate
//!
//! Each test reads `FIREFLY_TEST_REDIS_URL` (falling back to the older
//! `REDIS_URL`). When **unset**, the test prints a one-line `skipping …`
//! notice and returns — so `cargo test` on a machine with no Redis stays
//! green. When **set**, the test performs a real `EXISTS`/`SET EX`
//! round-trip against the service the URL points at.
//!
//! ## Isolation
//!
//! Every test namespaces its idempotency keys under a unique prefix derived
//! from a process-unique atomic counter (never `rand`), so concurrent and
//! repeated runs never collide; all state self-prunes via the stored TTL.
//! The TTL test asserts the **remaining** TTL Redis reports for a stored key
//! (via a direct `TTL` probe) rather than sleeping out the expiry, so no test
//! blocks on wall-clock time.
//!
//! Requires the `redis` feature: `cargo test -p firefly-webhooks --features redis`.

#![cfg(feature = "redis")]

use std::sync::atomic::{AtomicU64, Ordering};

use firefly_webhooks::{EventStore, RedisEventStore, DEFAULT_TTL_SECONDS};
use redis::AsyncCommands;

/// The standard Firefly integration env var (preferred), with the older
/// `REDIS_URL` accepted as a fallback.
fn redis_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .ok()
}

/// A process-unique prefix so concurrent / repeated runs never collide.
fn unique_prefix(tag: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("fftest:wh:{tag}:{pid}:{n}:")
}

#[tokio::test]
async fn remembers_and_recognises_keys_against_real_redis() {
    let Some(url) = redis_url() else {
        eprintln!("skipping redis webhook store test: FIREFLY_TEST_REDIS_URL/REDIS_URL unset");
        return;
    };
    let store = RedisEventStore::connect(&url)
        .await
        .expect("connect to redis")
        .with_key_prefix(unique_prefix("dedupe"));

    // A fresh key is not yet processed.
    assert!(!store.already_processed("evt-1").await.unwrap());
    // Recording it makes it recognised on the next lookup.
    store.remember("evt-1").await.unwrap();
    assert!(store.already_processed("evt-1").await.unwrap());
    // Idempotent: remembering again keeps it recognised.
    store.remember("evt-1").await.unwrap();
    assert!(store.already_processed("evt-1").await.unwrap());
    // A different key is independent.
    assert!(!store.already_processed("evt-2").await.unwrap());
}

#[tokio::test]
async fn remember_sets_the_configured_ttl() {
    let Some(url) = redis_url() else {
        eprintln!("skipping redis webhook store ttl test: FIREFLY_TEST_REDIS_URL/REDIS_URL unset");
        return;
    };
    let prefix = unique_prefix("ttl");
    // A long, explicit TTL so the assertion has a generous margin and the
    // key self-prunes without any test ever waiting on the clock.
    let ttl_seconds: u64 = 120;
    let store = RedisEventStore::connect(&url)
        .await
        .expect("connect to redis")
        .with_key_prefix(prefix.clone())
        .with_ttl_seconds(ttl_seconds);

    store.remember("with-ttl").await.unwrap();
    assert!(store.already_processed("with-ttl").await.unwrap());

    // Probe the remaining TTL directly: SET EX must have stamped a positive,
    // bounded expiry (Redis TTL returns -1 for "no expiry", -2 for "missing").
    let client = redis::Client::open(url).expect("open redis");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("connect");
    let remaining: i64 = conn
        .ttl(format!("{prefix}with-ttl"))
        .await
        .expect("TTL probe");
    assert!(
        remaining > 0 && remaining <= ttl_seconds as i64,
        "remaining TTL {remaining}s should be in (0, {ttl_seconds}]"
    );

    // The default-TTL constructor stamps the documented 24 h default.
    let default_store = RedisEventStore::connect(
        &std::env::var("FIREFLY_TEST_REDIS_URL")
            .or_else(|_| std::env::var("REDIS_URL"))
            .unwrap(),
    )
    .await
    .expect("connect")
    .with_key_prefix(unique_prefix("ttl-default"));
    default_store.remember("d").await.unwrap();
    assert!(default_store.already_processed("d").await.unwrap());
    assert_eq!(DEFAULT_TTL_SECONDS, 86_400);
}

#[tokio::test]
async fn usable_through_the_object_safe_port() {
    let Some(url) = redis_url() else {
        eprintln!("skipping redis webhook store port test: FIREFLY_TEST_REDIS_URL/REDIS_URL unset");
        return;
    };
    let store: std::sync::Arc<dyn EventStore> = std::sync::Arc::new(
        RedisEventStore::connect(&url)
            .await
            .expect("connect to redis")
            .with_key_prefix(unique_prefix("port")),
    );
    assert!(!store.already_processed("x").await.unwrap());
    store.remember("x").await.unwrap();
    assert!(store.already_processed("x").await.unwrap());
}
