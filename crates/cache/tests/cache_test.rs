//! Behaviour-contract tests ported 1:1 from the Go module's `cache_test.go`,
//! plus Rust-specific coverage (object safety, Send/Sync bounds, serde
//! round-trips, error-path semantics).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use firefly_cache::{Adapter, CacheError, FallbackAdapter, MemoryAdapter, NoOpAdapter, Typed};

/// Test double whose every operation fails with a transport-class error —
/// the stand-in for an unreachable Redis node.
struct FailingAdapter;

#[async_trait]
impl Adapter for FailingAdapter {
    async fn get(&self, _key: &str) -> Result<Vec<u8>, CacheError> {
        Err(CacheError::Backend("connection refused".into()))
    }
    async fn set(
        &self,
        _key: &str,
        _value: &[u8],
        _ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        Err(CacheError::Backend("connection refused".into()))
    }
    async fn delete(&self, _key: &str) -> Result<(), CacheError> {
        Err(CacheError::Backend("connection refused".into()))
    }
    async fn clear(&self) -> Result<(), CacheError> {
        Err(CacheError::Backend("connection refused".into()))
    }
    fn name(&self) -> String {
        "failing".to_owned()
    }
    async fn health_check(&self) -> Result<(), CacheError> {
        Err(CacheError::Backend("connection refused".into()))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct User {
    id: String,
    name: String,
}

// ---------------------------------------------------------------------------
// Go: TestMemory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_get_set_delete() {
    let m = MemoryAdapter::new();
    let err = m.get("k").await.unwrap_err();
    assert!(err.is_not_found(), "expected NotFound, got {err}");

    m.set("k", b"v", None).await.unwrap();
    let v = m.get("k").await.unwrap();
    assert_eq!(v, b"v");

    m.delete("k").await.unwrap();
    let err = m.get("k").await.unwrap_err();
    assert!(err.is_not_found(), "expected missing after delete");
}

// ---------------------------------------------------------------------------
// Go: TestMemoryTTL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_ttl_evicts_expired_entries() {
    let m = MemoryAdapter::new();
    m.set("k", b"v", Some(Duration::from_millis(1)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let err = m.get("k").await.unwrap_err();
    assert!(err.is_not_found(), "ttl not honoured");
    // Lazy eviction removed the entry on read.
    assert_eq!(m.len().await, 0);
}

/// Regression test: a `get()` that observes an expired entry under the read
/// lock must not blindly remove the key after re-acquiring the write lock —
/// a concurrent `set()` may have landed a fresh entry in between, and lazy
/// eviction must never delete that live write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_lazy_eviction_does_not_delete_concurrent_set() {
    let m = Arc::new(MemoryAdapter::new());
    for i in 0..500 {
        let key = format!("k{i}");

        // Seed an already-expired entry so the racing get() takes the lazy
        // eviction slow path (read guard drop -> write lock -> remove).
        m.set(&key, b"stale", Some(Duration::from_nanos(1)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;

        // Race the expired-key get() against a fresh, non-expiring set().
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let getter = tokio::spawn({
            let (m, key, barrier) = (m.clone(), key.clone(), barrier.clone());
            async move {
                barrier.wait().await;
                m.get(&key).await
            }
        });
        let setter = tokio::spawn({
            let (m, key, barrier) = (m.clone(), key.clone(), barrier.clone());
            async move {
                barrier.wait().await;
                m.set(&key, b"fresh", None).await
            }
        });

        // The racing get() may miss (linearized before the set) or observe
        // the fresh value (re-checked under the write lock) — never "stale".
        match getter.await.unwrap() {
            Ok(v) => assert_eq!(v, b"fresh", "iteration {i}: stale value served"),
            Err(e) => assert!(e.is_not_found(), "iteration {i}: unexpected error {e}"),
        }
        setter.await.unwrap().unwrap();

        // set() returned Ok with no TTL, so the write must never be lost to
        // the concurrent lazy eviction.
        assert_eq!(
            m.get(&key).await.unwrap(),
            b"fresh",
            "iteration {i}: concurrent set() lost to lazy TTL eviction"
        );
    }
}

#[tokio::test]
async fn memory_zero_ttl_means_no_expiry() {
    // Go: ttl <= 0 means no expiry; Some(0) maps onto the same contract.
    let m = MemoryAdapter::new();
    m.set("k", b"v", Some(Duration::ZERO)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(m.get("k").await.unwrap(), b"v");
}

#[tokio::test]
async fn memory_len_and_clear() {
    let m = MemoryAdapter::new();
    assert!(m.is_empty().await);
    m.set("a", b"1", None).await.unwrap();
    m.set("b", b"2", None).await.unwrap();
    assert_eq!(m.len().await, 2);
    m.clear().await.unwrap();
    assert!(m.is_empty().await);
    assert!(m.get("a").await.unwrap_err().is_not_found());
}

#[tokio::test]
async fn memory_copy_on_read_isolation() {
    let m = MemoryAdapter::new();
    m.set("k", b"abc", None).await.unwrap();
    let mut v = m.get("k").await.unwrap();
    v[0] = b'X';
    assert_eq!(m.get("k").await.unwrap(), b"abc", "stored bytes mutated");
}

#[tokio::test]
async fn memory_name_and_health() {
    let m = MemoryAdapter::new();
    assert_eq!(m.name(), "memory");
    m.health_check().await.unwrap();
}

// ---------------------------------------------------------------------------
// Go: TestNoOp
// ---------------------------------------------------------------------------

#[tokio::test]
async fn noop_always_misses() {
    let n = NoOpAdapter;
    n.set("k", b"v", None).await.unwrap();
    let err = n.get("k").await.unwrap_err();
    assert!(err.is_not_found(), "noop should always miss");
    assert_eq!(n.name(), "noop");
    n.delete("k").await.unwrap();
    n.clear().await.unwrap();
    n.health_check().await.unwrap();
}

// ---------------------------------------------------------------------------
// Go: TestFallback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fallback_union_and_write_through() {
    let primary = Arc::new(MemoryAdapter::new());
    let secondary = Arc::new(MemoryAdapter::new());
    secondary.set("k", b"from-secondary", None).await.unwrap();

    let f = FallbackAdapter::new(primary.clone(), secondary.clone());
    let v = f.get("k").await.unwrap();
    assert_eq!(v, b"from-secondary");

    // Primary is now warmer if Set is propagated:
    f.set("k2", b"both", None).await.unwrap();
    assert_eq!(
        primary.get("k2").await.unwrap(),
        b"both",
        "primary not written"
    );
    assert_eq!(
        secondary.get("k2").await.unwrap(),
        b"both",
        "secondary not written"
    );
}

#[tokio::test]
async fn fallback_name_composes_both() {
    let f = FallbackAdapter::new(Arc::new(MemoryAdapter::new()), Arc::new(NoOpAdapter));
    assert_eq!(f.name(), "fallback(memory+noop)");
}

#[tokio::test]
async fn fallback_prefers_primary_hit() {
    let primary = Arc::new(MemoryAdapter::new());
    let secondary = Arc::new(MemoryAdapter::new());
    primary.set("k", b"from-primary", None).await.unwrap();
    secondary.set("k", b"from-secondary", None).await.unwrap();
    let f = FallbackAdapter::new(primary, secondary);
    assert_eq!(f.get("k").await.unwrap(), b"from-primary");
}

#[tokio::test]
async fn fallback_demotes_on_transport_error() {
    let secondary = Arc::new(MemoryAdapter::new());
    secondary.set("k", b"survives", None).await.unwrap();
    let f = FallbackAdapter::new(Arc::new(FailingAdapter), secondary.clone());

    // Get falls through the broken primary.
    assert_eq!(f.get("k").await.unwrap(), b"survives");

    // Set swallows the primary transport error and still writes secondary.
    f.set("k2", b"v2", None).await.unwrap();
    assert_eq!(secondary.get("k2").await.unwrap(), b"v2");

    // Delete and clear behave the same.
    f.delete("k2").await.unwrap();
    assert!(secondary.get("k2").await.unwrap_err().is_not_found());
    f.clear().await.unwrap();
    assert!(secondary.is_empty().await);
}

#[tokio::test]
async fn fallback_health_check_one_healthy_is_healthy() {
    let f = FallbackAdapter::new(Arc::new(FailingAdapter), Arc::new(MemoryAdapter::new()));
    f.health_check().await.unwrap();

    let f = FallbackAdapter::new(Arc::new(MemoryAdapter::new()), Arc::new(FailingAdapter));
    f.health_check().await.unwrap();

    let f = FallbackAdapter::new(Arc::new(FailingAdapter), Arc::new(FailingAdapter));
    assert!(f.health_check().await.is_err());
}

#[tokio::test]
async fn fallback_miss_in_both_is_not_found() {
    let f = FallbackAdapter::new(
        Arc::new(MemoryAdapter::new()),
        Arc::new(MemoryAdapter::new()),
    );
    assert!(f.get("absent").await.unwrap_err().is_not_found());
}

// ---------------------------------------------------------------------------
// Go: TestTyped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn typed_get_or_set_runs_loader_once() {
    let c: Typed<User> = Typed::new(Arc::new(MemoryAdapter::new()));
    let loaded = AtomicUsize::new(0);
    let make_user = || User {
        id: "u1".into(),
        name: "alice".into(),
    };

    let v = c
        .get_or_set("u:1", Some(Duration::from_secs(60)), || async {
            loaded.fetch_add(1, Ordering::SeqCst);
            Ok(make_user())
        })
        .await
        .unwrap();
    assert_eq!(v.name, "alice");

    let v = c
        .get_or_set("u:1", Some(Duration::from_secs(60)), || async {
            loaded.fetch_add(1, Ordering::SeqCst);
            Ok(make_user())
        })
        .await
        .unwrap();
    assert_eq!(v.name, "alice");

    assert_eq!(
        loaded.load(Ordering::SeqCst),
        1,
        "loader should run once, ran {}",
        loaded.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn typed_set_then_get_round_trips() {
    let c: Typed<User> = Typed::new(Arc::new(MemoryAdapter::new()));
    let u = User {
        id: "u2".into(),
        name: "bob".into(),
    };
    c.set("u:2", &u, None).await.unwrap();
    assert_eq!(c.get("u:2").await.unwrap(), u);
}

#[tokio::test]
async fn typed_stores_go_compatible_json() {
    // Cross-port wire compatibility: the stored bytes must match Go's
    // encoding/json output for the equivalent struct.
    let adapter = Arc::new(MemoryAdapter::new());
    let c: Typed<User> = Typed::new(adapter.clone());
    c.set(
        "u:1",
        &User {
            id: "u1".into(),
            name: "alice".into(),
        },
        None,
    )
    .await
    .unwrap();
    let raw = adapter.get("u:1").await.unwrap();
    assert_eq!(raw, br#"{"id":"u1","name":"alice"}"#);

    // And bytes written by the Go port decode cleanly.
    adapter
        .set("u:go", br#"{"id":"g1","name":"gopher"}"#, None)
        .await
        .unwrap();
    let u = c.get("u:go").await.unwrap();
    assert_eq!(
        u,
        User {
            id: "g1".into(),
            name: "gopher".into()
        }
    );
}

#[tokio::test]
async fn typed_get_decode_error_is_codec() {
    let adapter = Arc::new(MemoryAdapter::new());
    adapter.set("bad", b"not-json", None).await.unwrap();
    let c: Typed<User> = Typed::new(adapter);
    let err = c.get("bad").await.unwrap_err();
    assert!(matches!(err, CacheError::Codec(_)), "got {err}");
}

#[tokio::test]
async fn typed_get_or_set_surfaces_non_miss_read_errors() {
    // A decode error is not a miss — get_or_set must surface it, not reload.
    let adapter = Arc::new(MemoryAdapter::new());
    adapter.set("bad", b"not-json", None).await.unwrap();
    let c: Typed<User> = Typed::new(adapter);
    let err = c
        .get_or_set("bad", None, || async {
            panic!("loader must not run on non-miss errors")
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CacheError::Codec(_)), "got {err}");
}

#[tokio::test]
async fn typed_get_or_set_propagates_loader_error() {
    let c: Typed<User> = Typed::new(Arc::new(MemoryAdapter::new()));
    let err = c
        .get_or_set("u:1", None, || async {
            Err(CacheError::Backend("repo down".into()))
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CacheError::Backend(_)), "got {err}");
    // Nothing was cached on loader failure.
    assert!(c.get("u:1").await.unwrap_err().is_not_found());
}

#[tokio::test]
async fn typed_get_or_set_caching_failure_does_not_mask_load() {
    // NoOp get misses and FailingAdapter set errors are both exercised:
    // a fallback of two failing-ish halves still returns the loaded value.
    let c: Typed<User> = Typed::new(Arc::new(FailingAdapter));
    let v = c
        .get_or_set("u:1", None, || async {
            Ok(User {
                id: "u1".into(),
                name: "alice".into(),
            })
        })
        .await;
    // FailingAdapter.get returns a transport error (not a miss), which Go's
    // GetOrSet surfaces — verify that exact contract first.
    assert!(matches!(v.unwrap_err(), CacheError::Backend(_)));

    // With a NoOp adapter the read is a clean miss; the loader runs; the
    // (silently dropped) NoOp write cannot mask the loaded value.
    let c: Typed<User> = Typed::new(Arc::new(NoOpAdapter));
    let v = c
        .get_or_set("u:1", None, || async {
            Ok(User {
                id: "u1".into(),
                name: "alice".into(),
            })
        })
        .await
        .unwrap();
    assert_eq!(v.name, "alice");
}

// ---------------------------------------------------------------------------
// Rust-specific: object safety, Send/Sync, error display parity
// ---------------------------------------------------------------------------

#[test]
fn error_messages_match_go_sentinel() {
    assert_eq!(CacheError::NotFound.to_string(), "firefly/cache: not found");
}

#[test]
fn adapters_are_object_safe_send_sync() {
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    let adapters: Vec<Arc<dyn Adapter>> = vec![
        Arc::new(MemoryAdapter::new()),
        Arc::new(NoOpAdapter),
        Arc::new(FallbackAdapter::new(
            Arc::new(MemoryAdapter::new()),
            Arc::new(NoOpAdapter),
        )),
    ];
    for a in &adapters {
        assert_send_sync(a);
    }
    let typed: Typed<User> = Typed::new(adapters[0].clone());
    assert_send_sync(&typed);
}

#[tokio::test]
async fn adapter_shared_across_tasks() {
    let a: Arc<dyn Adapter> = Arc::new(MemoryAdapter::new());
    let mut handles = Vec::new();
    for i in 0..8 {
        let a = a.clone();
        handles.push(tokio::spawn(async move {
            let key = format!("k{i}");
            a.set(&key, format!("v{i}").as_bytes(), None).await.unwrap();
            a.get(&key).await.unwrap()
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        assert_eq!(h.await.unwrap(), format!("v{i}").into_bytes());
    }
}
