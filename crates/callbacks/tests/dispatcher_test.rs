//! Dispatcher tests ported 1:1 from the Go module's
//! `core/dispatcher_test.go`, plus Rust-specific coverage of the wire
//! headers, signature bytes, filtering, and failure reporting. Each
//! test spawns a local axum receiver on port 0 — the
//! `httptest.NewServer` analog.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
use http::StatusCode;

use firefly_callbacks::{
    AuthorizedDomain, CallbackEvent, Dispatcher, DispatcherConfig, HmacDispatcher, MemoryStore,
    Store, Target, HEADER_EVENT, HEADER_EVENT_ID, HEADER_SIGNATURE, HEADER_TIMESTAMP,
};

/// Binds an axum router on a random localhost port and returns the base
/// URL — the `httptest.NewServer` analog.
async fn spawn_receiver(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}

/// A fast retry schedule so retry tests stay well under 200 ms.
const FAST_DELAY: Duration = Duration::from_millis(1);

fn expected_signature(secret: &[u8], payload: &[u8]) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).expect("hmac key");
    mac.update(payload);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

// --- Go: TestDispatcherSignsAndRetries -------------------------------------

#[tokio::test]
async fn dispatcher_signs_and_retries() {
    #[derive(Default)]
    struct Seen {
        sig: String,
        body: String,
        headers: Option<HeaderMap>,
    }
    let hits = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Seen::default()));
    let app = {
        let hits = hits.clone();
        let seen = seen.clone();
        Router::new().route(
            "/",
            post(move |headers: HeaderMap, body: Bytes| {
                let hits = hits.clone();
                let seen = seen.clone();
                async move {
                    let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    {
                        let mut s = seen.lock().unwrap();
                        s.sig = headers
                            .get(HEADER_SIGNATURE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        s.body = String::from_utf8_lossy(&body).into_owned();
                        s.headers = Some(headers);
                    }
                    if n < 2 {
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::OK
                    }
                }
            }),
        )
    };
    let url = spawn_receiver(app).await;

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "t1".into(),
            url,
            secret: "s3cret".into(),
            active: true,
            created_at: chrono::Utc::now(),
            ..Target::default()
        })
        .await
        .unwrap();
    let dispatcher = HmacDispatcher::new(
        store.clone(),
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 3,
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "ev1".into(),
            event_type: "x".into(),
            payload: br#"{"a":1}"#.to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .expect("dispatch");

    let n = hits.load(Ordering::SeqCst);
    {
        let s = seen.lock().unwrap();
        assert!(n >= 2, "hits={n}");
        assert_eq!(s.body, r#"{"a":1}"#);
        assert!(!s.sig.is_empty(), "no HMAC signature header");
        // Rust extra: the signature bytes themselves match the Go encoding.
        assert_eq!(s.sig, expected_signature(b"s3cret", br#"{"a":1}"#));
        // Rust extra: the remaining wire headers.
        let headers = s.headers.as_ref().unwrap();
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
        assert_eq!(headers.get(HEADER_EVENT).unwrap(), "x");
        assert_eq!(headers.get(HEADER_EVENT_ID).unwrap(), "ev1");
        let ts: i64 = headers
            .get(HEADER_TIMESTAMP)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .expect("unix-seconds timestamp header");
        assert!((ts - chrono::Utc::now().timestamp()).abs() < 60);
    }

    // Audit trail recorded.
    let attempts = store.list_attempts("ev1").await.unwrap();
    assert!(attempts.len() >= 2, "attempts: {attempts:?}");
    assert_eq!(attempts[0].status, 500);
    assert_eq!(attempts[0].attempt, 1);
    assert_eq!(attempts[1].status, 200);
    assert_eq!(attempts[1].attempt, 2);
    assert_eq!(attempts[0].event_id, "ev1");
    assert_eq!(attempts[0].target_id, "t1");
    assert_eq!(attempts[0].id.len(), 24, "Go newID format: 12 bytes hex");
}

// --- Go: TestDispatcherMatchesEventTypes ------------------------------------

#[tokio::test]
async fn dispatcher_matches_event_types() {
    let matched_hits = Arc::new(AtomicU32::new(0));
    let other_hits = Arc::new(AtomicU32::new(0));
    let counting = |hits: Arc<AtomicU32>| {
        Router::new().route(
            "/",
            post(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        )
    };
    let matched_url = spawn_receiver(counting(matched_hits.clone())).await;
    let other_url = spawn_receiver(counting(other_hits.clone())).await;

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "matched".into(),
            url: matched_url,
            active: true,
            event_types: vec!["order.created".into()],
            ..Target::default()
        })
        .await
        .unwrap();
    store
        .upsert_target(Target {
            id: "other".into(),
            url: other_url,
            active: true,
            event_types: vec!["order.shipped".into()],
            ..Target::default()
        })
        .await
        .unwrap();

    let dispatcher = HmacDispatcher::new(
        store,
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 1,
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e1".into(),
            event_type: "order.created".into(),
            payload: b"{}".to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .unwrap();

    assert_eq!(matched_hits.load(Ordering::SeqCst), 1);
    assert_eq!(other_hits.load(Ordering::SeqCst), 0);
}

// --- Rust-specific coverage --------------------------------------------------

#[tokio::test]
async fn inactive_targets_and_unsigned_deliveries() {
    let hits = Arc::new(AtomicU32::new(0));
    let signature_seen = Arc::new(Mutex::new(Option::<String>::None));
    let correlation_seen = Arc::new(Mutex::new(Option::<String>::None));
    let custom_seen = Arc::new(Mutex::new(Option::<String>::None));
    let app = {
        let hits = hits.clone();
        let signature_seen = signature_seen.clone();
        let correlation_seen = correlation_seen.clone();
        let custom_seen = custom_seen.clone();
        Router::new().route(
            "/",
            post(move |headers: HeaderMap| {
                let hits = hits.clone();
                let signature_seen = signature_seen.clone();
                let correlation_seen = correlation_seen.clone();
                let custom_seen = custom_seen.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let grab = |name: &str| {
                        headers
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string)
                    };
                    *signature_seen.lock().unwrap() = grab(HEADER_SIGNATURE);
                    *correlation_seen.lock().unwrap() = grab("X-Correlation-Id");
                    *custom_seen.lock().unwrap() = grab("X-Tenant");
                    StatusCode::OK
                }
            }),
        )
    };
    let url = spawn_receiver(app).await;

    let store = Arc::new(MemoryStore::new());
    // No secret, custom header, active: delivered without a signature.
    let mut custom = HashMap::new();
    custom.insert("X-Tenant".to_string(), "acme".to_string());
    store
        .upsert_target(Target {
            id: "unsigned".into(),
            url: url.clone(),
            active: true,
            headers: custom,
            ..Target::default()
        })
        .await
        .unwrap();
    // Inactive: never delivered.
    store
        .upsert_target(Target {
            id: "inactive".into(),
            url,
            secret: "irrelevant".into(),
            active: false,
            ..Target::default()
        })
        .await
        .unwrap();

    let dispatcher = HmacDispatcher::new(
        store.clone(),
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 1,
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e1".into(),
            event_type: "anything".into(),
            payload: b"{}".to_vec(),
            correlation_id: "corr-7".into(),
            ..CallbackEvent::default()
        })
        .await
        .unwrap();

    assert_eq!(hits.load(Ordering::SeqCst), 1, "inactive target skipped");
    assert_eq!(
        *signature_seen.lock().unwrap(),
        None,
        "no signature without a secret"
    );
    assert_eq!(
        correlation_seen.lock().unwrap().as_deref(),
        Some("corr-7"),
        "correlation id forwarded"
    );
    assert_eq!(
        custom_seen.lock().unwrap().as_deref(),
        Some("acme"),
        "target headers stamped"
    );
    // No attempts for the inactive target.
    let attempts = store.list_attempts("e1").await.unwrap();
    assert!(attempts.iter().all(|a| a.target_id == "unsigned"));
}

#[tokio::test]
async fn dispatch_swallows_per_target_failures_but_audits_them() {
    let app = Router::new().route("/", post(|| async { StatusCode::SERVICE_UNAVAILABLE }));
    let url = spawn_receiver(app).await;

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "failing".into(),
            url,
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();

    let dispatcher = HmacDispatcher::new(
        store.clone(),
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 3,
            ..DispatcherConfig::default()
        },
    );
    // Per-target failures are best-effort: Dispatch still returns Ok.
    dispatcher
        .dispatch(CallbackEvent {
            id: "e-fail".into(),
            event_type: "x".into(),
            payload: b"{}".to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .expect("dispatch is best-effort per target");

    let attempts = store.list_attempts("e-fail").await.unwrap();
    assert_eq!(attempts.len(), 3, "one audit row per attempt");
    assert!(attempts.iter().all(|a| a.status == 503));
    assert_eq!(
        attempts.iter().map(|a| a.attempt).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(attempts.iter().all(|a| a.finished_at >= a.started_at));
}

#[tokio::test]
async fn transport_failure_records_status_zero_attempts() {
    // A port that nothing listens on: connection refused.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "dead".into(),
            url: dead_url,
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();

    let dispatcher = HmacDispatcher::new(
        store.clone(),
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 2,
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e-dead".into(),
            event_type: "x".into(),
            payload: b"{}".to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .unwrap();

    let attempts = store.list_attempts("e-dead").await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.iter().all(|a| a.status == 0), "Go status 0");
    assert!(attempts.iter().all(|a| !a.error.is_empty()));
}

#[tokio::test]
async fn empty_event_types_matches_every_type() {
    let hits = Arc::new(AtomicU32::new(0));
    let app = {
        let hits = hits.clone();
        Router::new().route(
            "/",
            post(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        )
    };
    let url = spawn_receiver(app).await;

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "all".into(),
            url,
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();

    let dispatcher = HmacDispatcher::new(
        store,
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 1,
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e1".into(),
            event_type: "totally.unsubscribed".into(),
            payload: b"{}".to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn response_body_is_captured_in_the_audit_row() {
    let app = Router::new().route(
        "/",
        post(|| async { (StatusCode::OK, String::from("ack")) }),
    );
    let url = spawn_receiver(app).await;

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "t1".into(),
            url,
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();
    let dispatcher = HmacDispatcher::new(
        store.clone(),
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 1,
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e1".into(),
            event_type: "x".into(),
            payload: b"{}".to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .unwrap();

    let attempts = store.list_attempts("e1").await.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].body, "ack");
    assert!(attempts[0].error.is_empty());
}

// --- pyfly parity: AuthorizedDomain allowlist (#190) -------------------------

// --- pyfly: test_unauthorized_domain_is_blocked ------------------------------

#[tokio::test]
async fn unauthorized_domain_is_blocked_and_audited() {
    // The receiver must NEVER be hit: the host is not on the allowlist.
    let hits = Arc::new(AtomicU32::new(0));
    let app = {
        let hits = hits.clone();
        Router::new().route(
            "/x",
            post(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        )
    };
    let url = spawn_receiver(app).await;
    // The receiver listens on 127.0.0.1, which is NOT the allowlisted host.
    let target_url = format!("{url}/x");

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "evil".into(),
            url: target_url,
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();

    let dispatcher = HmacDispatcher::new(
        store.clone(),
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 3,
            authorized_domains: vec![AuthorizedDomain::new("trusted.example.com")],
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e1".into(),
            event_type: "E".into(),
            payload: br#"{"a":1}"#.to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .expect("dispatch is best-effort");

    // No HTTP request was made.
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "blocked target must not be hit"
    );

    // The rejection was audited (pyfly #190): one row, status 0, attempt 0,
    // with the explanatory error.
    let attempts = store.list_attempts("e1").await.unwrap();
    assert_eq!(attempts.len(), 1, "exactly one rejection audit row");
    assert_eq!(attempts[0].status, 0);
    assert_eq!(attempts[0].attempt, 0, "no delivery attempt was made");
    assert!(
        attempts[0].error.to_lowercase().contains("authorized"),
        "error: {:?}",
        attempts[0].error
    );
    assert_eq!(attempts[0].target_id, "evil");
}

#[tokio::test]
async fn authorized_domain_is_delivered_when_allowlisted() {
    let hits = Arc::new(AtomicU32::new(0));
    let app = {
        let hits = hits.clone();
        Router::new().route(
            "/",
            post(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        )
    };
    let url = spawn_receiver(app).await;
    // Derive the receiver's host (127.0.0.1) and allowlist it exactly.
    let host = url
        .strip_prefix("http://")
        .and_then(|hp| hp.split(':').next())
        .expect("host")
        .to_string();

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "ok".into(),
            url,
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();

    let dispatcher = HmacDispatcher::new(
        store.clone(),
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 1,
            authorized_domains: vec![AuthorizedDomain::new(host)],
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e1".into(),
            event_type: "E".into(),
            payload: b"{}".to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .unwrap();

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "allowlisted target delivered"
    );
    let attempts = store.list_attempts("e1").await.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, 200);
    assert_eq!(attempts[0].attempt, 1);
}

#[tokio::test]
async fn empty_allowlist_preserves_unrestricted_delivery() {
    // Backward compatibility: an empty allowlist (the default) reaches
    // any host, exactly as before this feature existed.
    let hits = Arc::new(AtomicU32::new(0));
    let app = {
        let hits = hits.clone();
        Router::new().route(
            "/",
            post(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        )
    };
    let url = spawn_receiver(app).await;

    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "any".into(),
            url,
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();

    // No authorized_domains configured.
    let dispatcher = HmacDispatcher::new(
        store,
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 1,
            ..DispatcherConfig::default()
        },
    );
    dispatcher
        .dispatch(CallbackEvent {
            id: "e1".into(),
            event_type: "E".into(),
            payload: b"{}".to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .unwrap();

    assert_eq!(hits.load(Ordering::SeqCst), 1, "unrestricted by default");
}

/// The dispatcher is usable through the object-safe [`Dispatcher`] port.
#[tokio::test]
async fn dispatcher_is_object_safe() {
    let store = Arc::new(MemoryStore::new());
    let dispatcher: Arc<dyn Dispatcher> = Arc::new(HmacDispatcher::new(
        store,
        DispatcherConfig {
            initial_delay: FAST_DELAY,
            max_attempts: 1,
            ..DispatcherConfig::default()
        },
    ));
    // No targets registered: a no-op dispatch.
    dispatcher.dispatch(CallbackEvent::default()).await.unwrap();
}
