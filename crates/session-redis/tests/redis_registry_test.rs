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

//! Always-on contract tests for
//! [`firefly_session_redis::RedisSessionRegistry`] against an in-process fake
//! RESP2 server (see `common/mod.rs`). These exercise the real Redis wire
//! protocol — `ZADD`/`ZRANGE WITHSCORES`/`ZREM`/`ZCARD`/`EXPIRE` — end to end
//! with **no external Redis**, so they run on every `cargo test`. The
//! env-gated live tests in `redis_integration_test.rs` cover a genuine server.

mod common;

use std::sync::Arc;

use common::FakeRedis;
use firefly_session::{ConcurrencyPolicy, SessionConcurrencyController, SessionRegistry, Strategy};
use firefly_session_redis::RedisSessionRegistry;

#[tokio::test]
async fn register_list_oldest_first_and_count() {
    let fake = FakeRedis::start().await;
    let reg = RedisSessionRegistry::connect(&fake.url())
        .await
        .expect("connect to fake redis");

    // Register out of creation order — list_sessions must still come back
    // oldest-first (by created_at score), like the in-memory registry.
    reg.register("alice", "s2", 2000).await;
    reg.register("alice", "s1", 1000).await;
    reg.register("alice", "s3", 3000).await;
    reg.register("bob", "b1", 500).await;

    assert_eq!(reg.count("alice").await, 3);
    assert_eq!(reg.count("bob").await, 1);

    let sessions = reg.list_sessions("alice").await;
    assert_eq!(
        sessions,
        vec![
            ("s1".to_owned(), 1000),
            ("s2".to_owned(), 2000),
            ("s3".to_owned(), 3000),
        ],
        "ZRANGE is ascending by score → oldest-first with the stored created_at"
    );
}

#[tokio::test]
async fn register_slides_the_ttl() {
    let fake = FakeRedis::start().await;
    let reg = RedisSessionRegistry::connect(&fake.url())
        .await
        .expect("connect");

    reg.register("carol", "s1", 1).await;

    // The adapter must EXPIRE the principal key with the default 24h TTL.
    let ttl = fake
        .state
        .lock()
        .unwrap()
        .last_expire
        .get("firefly:session:user:carol")
        .copied();
    assert_eq!(ttl, Some(86_400));
}

#[tokio::test]
async fn custom_ttl_zero_skips_expire() {
    let fake = FakeRedis::start().await;
    // ttl_secs <= 0 disables the EXPIRE entirely.
    let reg = RedisSessionRegistry::connect_with(&fake.url(), "firefly:session:user:", 0)
        .await
        .expect("connect");

    reg.register("dave", "s1", 1).await;

    assert!(
        !fake
            .state
            .lock()
            .unwrap()
            .last_expire
            .contains_key("firefly:session:user:dave"),
        "a non-positive TTL must not issue EXPIRE"
    );
}

#[tokio::test]
async fn deregister_is_idempotent_and_prunes_empty_set() {
    let fake = FakeRedis::start().await;
    let reg = RedisSessionRegistry::connect(&fake.url())
        .await
        .expect("connect");

    reg.register("erin", "s1", 1).await;
    reg.register("erin", "s2", 2).await;
    assert_eq!(reg.count("erin").await, 2);

    reg.deregister("erin", "s1").await;
    assert_eq!(reg.count("erin").await, 1);
    assert_eq!(reg.list_sessions("erin").await, vec![("s2".to_owned(), 2)]);

    // Deregistering an absent member is a no-op (idempotent).
    reg.deregister("erin", "s1").await;
    assert_eq!(reg.count("erin").await, 1);

    // Removing the last session prunes the principal's key entirely.
    reg.deregister("erin", "s2").await;
    assert_eq!(reg.count("erin").await, 0);
    assert!(reg.list_sessions("erin").await.is_empty());
    assert!(
        !fake
            .state
            .lock()
            .unwrap()
            .sets
            .contains_key("firefly:session:user:erin"),
        "an emptied sorted set must be dropped, like Redis"
    );
}

#[tokio::test]
async fn register_updates_existing_session_score() {
    let fake = FakeRedis::start().await;
    let reg = RedisSessionRegistry::connect(&fake.url())
        .await
        .expect("connect");

    reg.register("frank", "s1", 1000).await;
    // Re-registering the same session id updates the created_at score, not a
    // duplicate member (ZADD upserts).
    reg.register("frank", "s1", 9000).await;

    assert_eq!(reg.count("frank").await, 1);
    assert_eq!(
        reg.list_sessions("frank").await,
        vec![("s1".to_owned(), 9000)]
    );
}

#[tokio::test]
async fn missing_principal_is_empty() {
    let fake = FakeRedis::start().await;
    let reg = RedisSessionRegistry::connect(&fake.url())
        .await
        .expect("connect");
    assert_eq!(reg.count("nobody").await, 0);
    assert!(reg.list_sessions("nobody").await.is_empty());
}

#[tokio::test]
async fn is_available_pings() {
    let fake = FakeRedis::start().await;
    let reg = RedisSessionRegistry::connect(&fake.url())
        .await
        .expect("connect");
    assert!(reg.is_available().await);
}

/// The distributed registry must satisfy the same concurrency-controller
/// contract the in-process registry does — evict-oldest holds the cap by
/// dropping the lowest-score (oldest) session.
#[tokio::test]
async fn evict_oldest_through_controller() {
    let fake = FakeRedis::start().await;
    let reg = Arc::new(
        RedisSessionRegistry::connect(&fake.url())
            .await
            .expect("connect"),
    );
    let ctl = SessionConcurrencyController::new(
        reg.clone(),
        ConcurrencyPolicy {
            max_sessions: 2,
            strategy: Strategy::EvictOldest,
        },
    );

    assert!(ctl.on_login("grace", "s1", 1).await);
    assert!(ctl.on_login("grace", "s2", 2).await);
    assert!(ctl.on_login("grace", "s3", 3).await); // evicts oldest (s1)

    assert_eq!(reg.count("grace").await, 2);
    let ids: Vec<String> = reg
        .list_sessions("grace")
        .await
        .into_iter()
        .map(|(sid, _)| sid)
        .collect();
    assert_eq!(ids, vec!["s2".to_owned(), "s3".to_owned()]);
}

/// Reject-new through the controller refuses the over-cap login and leaves the
/// existing distributed index untouched.
#[tokio::test]
async fn reject_new_through_controller() {
    let fake = FakeRedis::start().await;
    let reg = Arc::new(
        RedisSessionRegistry::connect(&fake.url())
            .await
            .expect("connect"),
    );
    let ctl = SessionConcurrencyController::new(
        reg.clone(),
        ConcurrencyPolicy {
            max_sessions: 2,
            strategy: Strategy::RejectNew,
        },
    );

    assert!(ctl.on_login("heidi", "s1", 1).await);
    assert!(ctl.on_login("heidi", "s2", 2).await);
    assert!(!ctl.on_login("heidi", "s3", 3).await); // over cap → rejected
    assert_eq!(reg.count("heidi").await, 2);
}
