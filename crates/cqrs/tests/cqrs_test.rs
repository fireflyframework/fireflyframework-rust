//! Port of the Go module's `cqrs_test.go`, plus Rust-specific coverage
//! (middleware ordering, TTL expiry, error pass-through, Send/Sync
//! bounds).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use firefly_cqrs::{
    Bus, CqrsError, DynHandler, Envelope, HandlerFuture, Message, Middleware, QueryCache,
    ValidationMiddleware,
};
use serde::Serialize;

// ---- fixtures (mirror cqrs_test.go) -----------------------------------

#[derive(Clone, Serialize)]
struct CreateUser {
    name: String,
}

impl Message for CreateUser {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.name.is_empty() {
            return Err(CqrsError::validation("name required"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
struct UserCreated {
    id: String,
    name: String,
}

#[derive(Clone, Serialize)]
struct GetUser {
    id: String,
}

impl Message for GetUser {
    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
}

fn create_user(name: &str) -> CreateUser {
    CreateUser { name: name.into() }
}

/// Registers a `GetUser` handler that counts invocations.
fn register_counting_get_user(bus: &Bus) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetUser| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(UserCreated {
                id: q.id,
                name: "alice".into(),
            })
        }
    });
    calls
}

// ---- ported Go tests ---------------------------------------------------

/// Go: TestCommandRoundTrip.
#[tokio::test]
async fn command_round_trip() {
    let bus = Bus::new();
    bus.register(|c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "u1".into(),
            name: c.name,
        })
    });
    let out: UserCreated = bus.send(create_user("alice")).await.unwrap();
    assert_eq!(out.id, "u1");
    assert_eq!(out.name, "alice");
}

/// Go: TestNoHandler.
#[tokio::test]
async fn no_handler() {
    let bus = Bus::new();
    let err = bus
        .send::<CreateUser, UserCreated>(create_user("x"))
        .await
        .unwrap_err();
    assert!(err.is_no_handler(), "expected NoHandler, got {err:?}");
    // Display parity with Go's `fmt.Errorf("%w: %T", ErrNoHandler, msg)`.
    let msg = err.to_string();
    assert!(
        msg.starts_with("firefly/cqrs: no handler registered: "),
        "unexpected message {msg:?}"
    );
    assert!(msg.contains("CreateUser"), "unexpected message {msg:?}");
}

/// Go: TestValidationMiddleware.
#[tokio::test]
async fn validation_middleware() {
    let bus = Bus::new();
    bus.use_middleware(ValidationMiddleware::new());
    bus.register(|c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: String::new(),
            name: c.name,
        })
    });

    let err = bus
        .send::<CreateUser, UserCreated>(create_user(""))
        .await
        .expect_err("expected validation failure");
    assert!(matches!(err, CqrsError::Validation(_)), "got {err:?}");
    // The validation error passes through verbatim, like Go.
    assert_eq!(err.to_string(), "name required");

    let out: UserCreated = bus.send(create_user("a")).await.unwrap();
    assert_eq!(out.name, "a");
}

/// Go: TestQueryCache — loader runs once, invalidation by type prefix
/// makes it run again.
#[tokio::test]
async fn query_cache() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = register_counting_get_user(&bus);

    for _ in 0..3 {
        let v: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
        assert_eq!(v.name, "alice");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "handler should run once while cached"
    );

    cache.invalidate_type::<GetUser>();
    let _: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "after invalidate handler should run again"
    );
}

// ---- Rust-specific coverage --------------------------------------------

/// `invalidate` with the raw type-name prefix (Go callers pass the
/// `reflect.Type` string, e.g. `"cqrs.GetUser"`).
#[tokio::test]
async fn query_cache_invalidate_by_string_prefix() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = register_counting_get_user(&bus);

    let _: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
    cache.invalidate(std::any::type_name::<GetUser>());
    let _: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// A non-matching prefix must leave entries untouched.
#[tokio::test]
async fn query_cache_invalidate_nonmatching_prefix_keeps_entries() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = register_counting_get_user(&bus);

    let _: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
    cache.invalidate("some.other.Query");
    let _: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cache entry must survive");
}

/// Distinct message values hash to distinct keys; equal values share one.
#[tokio::test]
async fn query_cache_keys_by_message_value() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = register_counting_get_user(&bus);

    let _: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
    let _: UserCreated = bus.query(GetUser { id: "u2".into() }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "distinct values miss");
    let _: UserCreated = bus.query(GetUser { id: "u1".into() }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "equal value hits");
}

#[derive(Clone, Serialize)]
struct GetUserShortTtl {
    id: String,
}

impl Message for GetUserShortTtl {
    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_millis(30))
    }
}

/// Entries past their TTL miss, matching Go's `time.Now().After(exp)`.
#[tokio::test]
async fn query_cache_ttl_expiry() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetUserShortTtl| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(UserCreated {
                id: q.id,
                name: "alice".into(),
            })
        }
    });

    let _: UserCreated = bus
        .query(GetUserShortTtl { id: "u1".into() })
        .await
        .unwrap();
    let _: UserCreated = bus
        .query(GetUserShortTtl { id: "u1".into() })
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "within TTL: hit");

    tokio::time::sleep(Duration::from_millis(50)).await;
    let _: UserCreated = bus
        .query(GetUserShortTtl { id: "u1".into() })
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "past TTL: reload");
}

#[derive(Clone, Serialize)]
struct GetUserZeroTtl {
    id: String,
}

impl Message for GetUserZeroTtl {
    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::ZERO)
    }
}

/// `Some(Duration::ZERO)` caches without expiry (Go: `ttl <= 0` leaves
/// the zero expiry time).
#[tokio::test]
async fn query_cache_zero_ttl_caches_without_expiry() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetUserZeroTtl| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(UserCreated {
                id: q.id,
                name: "alice".into(),
            })
        }
    });

    let _: UserCreated = bus.query(GetUserZeroTtl { id: "u1".into() }).await.unwrap();
    let _: UserCreated = bus.query(GetUserZeroTtl { id: "u1".into() }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Failed dispatches are never cached (Go: `if err != nil { return res,
/// err }` before `c.set`).
#[tokio::test]
async fn query_cache_does_not_cache_errors() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |_q: GetUser| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Err::<UserCreated, _>(CqrsError::handler("boom"))
        }
    });

    for _ in 0..2 {
        let err = bus
            .query::<GetUser, UserCreated>(GetUser { id: "u1".into() })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2, "errors must not be cached");
}

/// Commands (no cache_ttl) bypass the query cache entirely.
#[tokio::test]
async fn query_cache_passes_non_cacheable_through() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |c: CreateUser| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(UserCreated {
                id: "u1".into(),
                name: c.name,
            })
        }
    });

    let _: UserCreated = bus.send(create_user("alice")).await.unwrap();
    let _: UserCreated = bus.send(create_user("alice")).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "commands are never cached");
}

/// With no handler registered, middleware never runs — the lookup fails
/// first, exactly like Go's dispatch.
#[tokio::test]
async fn no_handler_short_circuits_before_middleware() {
    let bus = Bus::new();
    bus.use_middleware(ValidationMiddleware::new());
    // Invalid message, but the error must be NoHandler, not Validation.
    let err = bus
        .send::<CreateUser, UserCreated>(create_user(""))
        .await
        .unwrap_err();
    assert!(err.is_no_handler(), "got {err:?}");
}

/// Registering twice for the same message type overwrites (documented Go
/// behaviour).
#[tokio::test]
async fn register_overwrites_previous_handler() {
    let bus = Bus::new();
    bus.register(|_c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "first".into(),
            name: String::new(),
        })
    });
    bus.register(|_c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "second".into(),
            name: String::new(),
        })
    });
    let out: UserCreated = bus.send(create_user("x")).await.unwrap();
    assert_eq!(out.id, "second");
}

/// Asking for the wrong result type reproduces Go's `result type
/// mismatch` guard.
#[tokio::test]
async fn result_type_mismatch() {
    let bus = Bus::new();
    bus.register(|c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "u1".into(),
            name: c.name,
        })
    });
    let err = bus
        .send::<CreateUser, String>(create_user("alice"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, CqrsError::ResultTypeMismatch { .. }),
        "got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.starts_with("firefly/cqrs: result type mismatch want "),
        "unexpected message {msg:?}"
    );
}

/// Recording middleware used to assert chain order.
struct Recorder {
    label: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Middleware for Recorder {
    fn wrap(&self, next: DynHandler) -> DynHandler {
        let label = self.label;
        let log = Arc::clone(&self.log);
        Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let next = Arc::clone(&next);
            let log = Arc::clone(&log);
            Box::pin(async move {
                log.lock().unwrap().push(label);
                next(env).await
            })
        })
    }
}

/// First-registered middleware wraps outermost (Go: `Use` doc, dispatch
/// loop wraps in reverse).
#[tokio::test]
async fn middleware_runs_in_registration_order() {
    let bus = Bus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    bus.use_middleware(Recorder {
        label: "outer",
        log: Arc::clone(&log),
    });
    bus.use_middleware(Recorder {
        label: "inner",
        log: Arc::clone(&log),
    });
    bus.register(|c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "u1".into(),
            name: c.name,
        })
    });

    let _: UserCreated = bus.send(create_user("alice")).await.unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["outer", "inner"]);
}

/// The bus is shareable across tasks — concurrent dispatch works.
#[tokio::test]
async fn concurrent_dispatch() {
    let bus = Arc::new(Bus::new());
    bus.register(|c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "u1".into(),
            name: c.name,
        })
    });

    let mut tasks = Vec::new();
    for i in 0..8 {
        let bus = Arc::clone(&bus);
        tasks.push(tokio::spawn(async move {
            bus.send::<CreateUser, UserCreated>(create_user(&format!("user-{i}")))
                .await
        }));
    }
    for (i, task) in tasks.into_iter().enumerate() {
        let out = task.await.unwrap().unwrap();
        assert_eq!(out.name, format!("user-{i}"));
    }
}

/// Compile-time Send + Sync guarantees for the shared surface.
#[test]
fn public_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Bus>();
    assert_send_sync::<QueryCache>();
    assert_send_sync::<ValidationMiddleware>();
    assert_send_sync::<CqrsError>();
    assert_send_sync::<Envelope>();
}

/// Error display strings match the Go module's wording.
#[test]
fn error_display_parity() {
    let err = CqrsError::NoHandler {
        type_name: "demo::CreateUser",
    };
    assert_eq!(
        err.to_string(),
        "firefly/cqrs: no handler registered: demo::CreateUser"
    );
    let err = CqrsError::HandlerTypeMismatch {
        want: "A",
        got: "B",
    };
    assert_eq!(
        err.to_string(),
        "firefly/cqrs: handler type mismatch want A got B"
    );
    let err = CqrsError::ResultTypeMismatch {
        want: "A",
        got: "B",
    };
    assert_eq!(
        err.to_string(),
        "firefly/cqrs: result type mismatch want A got B"
    );
    assert_eq!(
        CqrsError::validation("name required").to_string(),
        "name required"
    );
    assert_eq!(CqrsError::handler("boom").to_string(), "boom");
}
