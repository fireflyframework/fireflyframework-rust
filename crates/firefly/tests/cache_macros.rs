// Integration test for the declarative cache macros — `#[firefly::cacheable]`,
// `#[firefly::cache_put]`, and `#[firefly::cache_evict]` — all routed through
// the one `firefly` facade. A `MemoryAdapter` is registered as the
// process-global cache; a `#[cacheable]` method increments a static counter so
// a second call with the same key proves the body did NOT run (cache hit), and
// a `#[cache_evict]` forces the next call to recompute.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once};

use firefly::cache::{register_cache, MemoryAdapter};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
struct DemoError;
impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "demo error")
    }
}
impl std::error::Error for DemoError {}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
struct Order {
    id: u64,
    total: u64,
}

// Per-id compute counters, so tests that own disjoint ids (1, 2, 3) can prove a
// cache hit without interfering with each other when run in parallel.
static COMPUTES: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// How many times the `load_order` body has run for `id`.
fn computes(id: u64) -> u64 {
    COMPUTES[id as usize].load(Ordering::SeqCst)
}

struct OrderService;

impl OrderService {
    #[firefly::cacheable(key = "format!(\"order:{}\", id)", ttl = "60s")]
    async fn load_order(&self, id: u64) -> Result<Order, DemoError> {
        COMPUTES[id as usize].fetch_add(1, Ordering::SeqCst);
        Ok(Order { id, total: id * 10 })
    }

    #[firefly::cache_evict(key = "format!(\"order:{}\", id)")]
    async fn delete_order(&self, id: u64) -> Result<(), DemoError> {
        Ok(())
    }

    #[firefly::cache_put(key = "format!(\"order:{}\", order.id)", ttl = "60s")]
    async fn save_order(&self, order: Order) -> Result<Order, DemoError> {
        Ok(order)
    }
}

// The process-global cache is first-wins, so register exactly once per test
// process regardless of which test runs first.
static INIT: Once = Once::new();
fn init_cache() {
    INIT.call_once(|| {
        register_cache(Arc::new(MemoryAdapter::new()));
    });
}

#[tokio::test]
async fn cacheable_second_call_is_a_hit_and_does_not_recompute() {
    init_cache();
    let svc = OrderService;

    // First call: cache miss, body runs exactly once for id 1.
    let first = svc.load_order(1).await.expect("first load");
    assert_eq!(first, Order { id: 1, total: 10 });
    assert_eq!(computes(1), 1, "first call must run the body once");

    // Second call, same key: cache hit, body must NOT run again.
    let second = svc.load_order(1).await.expect("second load");
    assert_eq!(second, first);
    assert_eq!(
        computes(1),
        1,
        "second call must be served from the cache without recomputing"
    );
}

#[tokio::test]
async fn cache_evict_forces_a_recompute() {
    init_cache();
    let svc = OrderService;

    // Prime the cache with key "order:2".
    let _ = svc.load_order(2).await.expect("prime");
    assert_eq!(computes(2), 1);
    // Confirm it is cached (no recompute).
    let _ = svc.load_order(2).await.expect("hit");
    assert_eq!(computes(2), 1);

    // Evict it; the next load must run the body again.
    svc.delete_order(2).await.expect("evict");
    let _ = svc.load_order(2).await.expect("recompute");
    assert_eq!(computes(2), 2, "after eviction the body must recompute");
}

#[tokio::test]
async fn cache_put_warms_the_cache_so_cacheable_reads_it() {
    init_cache();
    let svc = OrderService;

    // `save_order` writes through under "order:3" without ever running the
    // `load_order` body.
    let saved = svc
        .save_order(Order { id: 3, total: 999 })
        .await
        .expect("save");
    assert_eq!(saved, Order { id: 3, total: 999 });

    // A subsequent `load_order(3)` is a hit (the put-stored value), so the
    // body never runs and we read the put value (total 999), not the
    // recomputed 30.
    let loaded = svc.load_order(3).await.expect("load after put");
    assert_eq!(
        loaded,
        Order { id: 3, total: 999 },
        "cacheable must serve the value cache_put stored"
    );
    assert_eq!(
        computes(3),
        0,
        "cache_put primed the cache, so load_order must not recompute"
    );
}
