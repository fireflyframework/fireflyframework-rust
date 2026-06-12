//! Ports the behaviour of pyfly's `Provider[T]` (`container/provider.py`) and
//! `RefreshScope` (`container/refresh_scope.py`), plus per-bean metrics.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use firefly_container::{Container, RefreshScope, Scope, REFRESH_SCOPE_NAME};

#[test]
fn provider_defers_resolution_and_yields_fresh_transient() {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    struct Job(u32);

    let c = Arc::new(Container::new());
    c.register_factory::<Job, _>(Scope::Transient, |_| {
        Ok(Job(COUNTER.fetch_add(1, Ordering::SeqCst)))
    });

    let provider = c.provider::<Job>();
    let a = provider.get().unwrap();
    let b = provider.get().unwrap();
    assert_ne!(a.0, b.0); // fresh transient each call
}

#[test]
fn provider_for_unregistered_errors_at_get_not_construction() {
    struct Missing;
    let c = Arc::new(Container::new());
    let provider = c.provider::<Missing>(); // construction does not fail
    assert!(provider.get().is_err()); // lookup fails only on use
}

#[test]
fn refresh_scope_caches_until_refreshed() {
    struct FeatureFlags(u32);
    static GEN: AtomicU32 = AtomicU32::new(0);

    let refresh = Arc::new(RefreshScope::new());
    let c = Container::new();
    c.register_scope(REFRESH_SCOPE_NAME, refresh.clone())
        .unwrap();
    c.register_factory_scoped::<FeatureFlags, _>(REFRESH_SCOPE_NAME, "", |_| {
        Ok(FeatureFlags(GEN.fetch_add(1, Ordering::SeqCst)))
    });

    let first = c.resolve::<FeatureFlags>().unwrap();
    let second = c.resolve::<FeatureFlags>().unwrap();
    assert!(Arc::ptr_eq(&first, &second)); // cached like a singleton

    let evicted = refresh.refresh();
    assert_eq!(evicted.len(), 1);

    let rebuilt = c.resolve::<FeatureFlags>().unwrap();
    assert!(!Arc::ptr_eq(&first, &rebuilt)); // rebuilt after refresh
    assert_ne!(first.0, rebuilt.0);
}

#[test]
fn bean_metrics_track_resolution_count() {
    #[derive(Default)]
    struct Counted;
    let c = Container::new();
    c.register::<Counted>();
    assert!(c.bean_metrics::<Counted>().unwrap().resolution_count == 0);
    let _ = c.resolve::<Counted>().unwrap();
    let _ = c.resolve::<Counted>().unwrap();
    let m = c.bean_metrics::<Counted>().unwrap();
    assert_eq!(m.resolution_count, 2);
}

#[test]
fn container_is_send_sync_and_shareable() {
    #[derive(Default)]
    struct Shared(u32);
    let c = Arc::new(Container::new());
    c.register::<Shared>();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let c = Arc::clone(&c);
            std::thread::spawn(move || c.resolve::<Shared>().unwrap())
        })
        .collect();

    // Join EVERY handle: `.map(..).next()` is lazy and would detach the other
    // seven threads, racing the resolution-count assertion below.
    let resolved: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    // All threads observe the same default singleton instance.
    assert!(resolved.iter().all(|s| s.0 == 0));
    assert_eq!(c.bean_metrics::<Shared>().unwrap().resolution_count, 8);
}
