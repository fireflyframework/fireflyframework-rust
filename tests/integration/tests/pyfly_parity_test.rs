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

//! Cross-module integration tests for the **pyfly-parity seams** — the
//! new crates the Rust port grew to match
//! `fireflyframework-pyfly`'s cross-cutting suite
//! (`tests/test_integration.py` + `tests/test_hexagonal.py` +
//! `tests/integration/`).
//!
//! Where the Go-parity suite (`integration_test.rs`) proves the original
//! callbacks/webhooks/saga/health seams, this file proves the pyfly
//! additions compose across **three or more** crates each:
//!
//! | Scenario | Crates exercised |
//! |----------|------------------|
//! | web CORS + headers + CSRF + metrics through starter-core | `firefly-web` + `firefly-starter-core` + `firefly-actuator` |
//! | JWKS bearer + FilterChain + RoleHierarchy | `firefly-security` + `firefly-web` (oneshot) |
//! | workflow persistence + wait-for-signal + recovery | `firefly-orchestration` |
//! | CQRS authorization + EDA cache invalidation | `firefly-cqrs` + `firefly-eda` |
//! | eventsourcing transactional outbox → eda broker | `firefly-eventsourcing` + `firefly-eda` |
//! | eda subscribe_group round-robin + wrap_listener DLQ | `firefly-eda` |
//! | notifications opt-out + template precedence | `firefly-notifications` |
//! | config placeholder + reload + property-source masking | `firefly-config` |
//!
//! Every collaborator is wired in-memory or driven through
//! `tower::ServiceExt::oneshot`; the only "server" is the JWKS endpoint,
//! a `tower` router answered by oneshot (no socket bound). No external
//! services, no sleeps over 200 ms.
//!
//! (The optional admin `mount()` smoke — scenario 9 in the brief — is
//! omitted: `firefly-admin`'s real surface is still pending (`lib.rs` is
//! a version-stamp placeholder with no `mount()`), so there is nothing to
//! oneshot yet. It will land here once admin ships its router.)

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Serialize;
use tower::ServiceExt;

use firefly_actuator::MetricRegistry;
use firefly_starter_core::{Core, CoreConfig};
use firefly_web::{
    CorsConfig, CorsLayer, CsrfLayer, MetricsLayer, RequestMetric, RequestObserver,
    SecurityHeadersConfig, SecurityHeadersLayer, CSRF_COOKIE_NAME, CSRF_HEADER_NAME,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async fn body_bytes(res: axum::response::Response) -> Vec<u8> {
    use http_body_util::BodyExt;
    res.into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

// ===========================================================================
// Scenario 1 — web CORS + security-headers + CSRF + request-metrics
//              through starter-core Core::apply_middleware
// ===========================================================================

/// Bridges the web [`RequestObserver`] onto the actuator
/// [`MetricRegistry`] — the in-test spelling of the bridge
/// `firefly-starter-core` would install, proving the web metrics seam
/// feeds the actuator registry surfaced on `/actuator/metrics`.
struct RegistryObserver {
    registry: Arc<MetricRegistry>,
}

impl RequestObserver for RegistryObserver {
    fn record(&self, metric: &RequestMetric) {
        self.registry
            .counter_with(
                "http_server_requests_total",
                &[
                    ("method", metric.method.as_str()),
                    ("uri", metric.uri.as_str()),
                    ("outcome", metric.outcome.as_str()),
                ],
            )
            .inc();
    }
}

/// The full pyfly web filter stack composes on top of starter-core's
/// canonical chain: CORS short-circuits preflight, the security-headers
/// filter stamps OWASP headers on every response, CSRF guards unsafe
/// methods with the double-submit cookie, and the metrics filter records
/// each request into the actuator [`MetricRegistry`]. One request flows
/// through all four plus starter-core's correlation/idempotency/problem
/// layers, and we assert the headers AND the registry counter.
#[tokio::test]
async fn web_cors_headers_csrf_and_metrics_compose_through_starter_core() {
    let core = Core::new(CoreConfig {
        app_name: "integration".into(),
        ..CoreConfig::default()
    });
    let registry = Arc::clone(&core.metrics);

    // A GET handler (safe method — CSRF sets the cookie, does not block).
    let app = Router::new().route("/orders", get(|| async { "[]" }));

    // Compose the pyfly web layers, then wrap in starter-core's canonical
    // middleware. Router::layer applies bottom-up, so the metrics filter
    // sees the matched route while CORS/headers/CSRF run around it.
    // A concrete origin allow-list (not "*") so the response echoes the
    // origin with a Vary header — the credential-safe CORS path.
    let cors = CorsConfig {
        allowed_origins: vec!["https://app.example.com".into()],
        allowed_methods: vec!["GET".into(), "POST".into()],
        ..CorsConfig::default()
    };
    let metrics_observer: Arc<dyn RequestObserver> = Arc::new(RegistryObserver {
        registry: Arc::clone(&registry),
    });
    let api = core.apply_middleware(
        app.layer(MetricsLayer::new(metrics_observer))
            .layer(CsrfLayer::new())
            .layer(SecurityHeadersLayer::new(SecurityHeadersConfig::default()))
            .layer(CorsLayer::new(cors)),
    );

    // --- 1a. CORS preflight is short-circuited with 200 + ACAO. ---
    let preflight = api
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/orders")
                .header("Origin", "https://app.example.com")
                .header("Access-Control-Request-Method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::OK, "CORS preflight");
    assert_eq!(
        preflight
            .headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap().to_owned()),
        Some("https://app.example.com".to_owned()),
        "CORS echoes the origin"
    );

    // --- 1b. A real GET flows through the whole stack. ---
    let res = api
        .clone()
        .oneshot(
            Request::get("/orders")
                .header("Origin", "https://app.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Security headers (OWASP defaults) are present.
    assert_eq!(
        res.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(res.headers().get("x-frame-options").unwrap(), "DENY");
    assert!(res.headers().get("referrer-policy").is_some());
    // CORS decorated the actual response too.
    assert_eq!(
        res.headers().get("access-control-allow-origin").unwrap(),
        "https://app.example.com"
    );
    // CSRF set the double-submit cookie on the safe-method response.
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        set_cookie.contains(CSRF_COOKIE_NAME),
        "CSRF cookie missing: {set_cookie:?}"
    );
    // starter-core's correlation layer minted/echoed a correlation id.
    assert!(res.headers().get("x-correlation-id").is_some());

    // --- 1c. The metrics filter recorded the request into the registry. ---
    let counter = registry.counter_with(
        "http_server_requests_total",
        &[
            ("method", "GET"),
            ("uri", "/orders"),
            ("outcome", "SUCCESS"),
        ],
    );
    assert_eq!(counter.get(), 1, "metrics filter fed the actuator registry");

    // --- 1d. CSRF blocks an unsafe method without the double-submit pair. ---
    let app2 = Router::new().route("/orders", post(|| async { StatusCode::CREATED }));
    let api2 = core.apply_middleware(app2.layer(CsrfLayer::new()));
    let denied = api2
        .clone()
        .oneshot(Request::post("/orders").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "CSRF blocks bare POST"
    );

    // The matching cookie+header pair is accepted.
    let allowed = api2
        .oneshot(
            Request::post("/orders")
                .header("cookie", format!("{CSRF_COOKIE_NAME}=tok-abc"))
                .header(CSRF_HEADER_NAME, "tok-abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        allowed.status(),
        StatusCode::CREATED,
        "CSRF accepts the double-submit pair"
    );
}

// ===========================================================================
// Scenario 2 — security JWKS verifier accepting a token minted against an
//              in-process JWKS document, gating a FilterChain-protected
//              route with RoleHierarchy
// ===========================================================================

mod jwks_fixture {
    //! A fixed RSA-2048 test keypair: the PKCS#1 PEM signs tokens, and the
    //! matching (n, e) base64url components feed the JWKS document. Using a
    //! frozen keypair keeps the test deterministic and dependency-free (no
    //! key-generation crate) — the same trick pyfly's resource-server tests
    //! use with a checked-in JWK.

    /// The signing key (PKCS#1 PEM). Test-only; never used outside this file.
    pub const PRIVATE_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\n\
MIIEowIBAAKCAQEA2AQHKy4lzOoachm0nq2icBbbC76rNzFts8j60FjDSypxrBAV\n\
2tFq963hQoD0N7Wz4+eqrTMgDOoTokUNBp7V5X8hbW4aJJaDFIvcN04TELaIMJ7M\n\
Z+vQOd9yMSBy0ybndx3w0En8MY0BQ5jZuwZhKiZyz42J6fm5zlCsvKWP8YmLpk92\n\
yP7l45TKOJhbJnL+biqKzPgydCwQv6+b+QPw4himhd+bWUF2ibwZ4AuNdS9CZGai\n\
f/yBivGmoQt5URhftEE84dabDoosevpvM0AEBz5wtiuZmMmRP+ERPFvgmnkH34TZ\n\
XI2Qg/5XpFw/wYc1JTB0C9YHBny2EI5L3FuLyQIDAQABAoIBAFvgCrSA4i7veuQA\n\
ruO2cho+flfWdrf42/HVj2fB+P4lRUerZ8Azxc0mNWK1uilfrO1IAT29OuqDPGqN\n\
9+ZS9CKyGaOTaqcZJRM+ESGsoUtAd1hGkiW5FE0pDkZ6yJuqNlRhdaUBcrQYhusb\n\
Pa/gHL9yru48AuZzAmYPxghOWpSd5R7XtnUNAQfYAOG/SKMKF6P4yleCaSn+HEUF\n\
nQMIyroJN7URwm4wuMesrGwbszj2Ac5Z1tTjgDKnoCiDQyHbyc+Dp058w0Zz+NF5\n\
8kM7PDllK+H6AYdYlKKlzvnDyTz0D9Ilx2cP4vJuX4H9Be76cskriDUSIyHxXm2k\n\
saYzbwECgYEA/YlpQwV0XE0BAY9YqDR2a5N1iRuqzpESH7+MXLF3cQ+bPZ1MN5PH\n\
iULVSSQfYvZnmq8r9Jt7lEf4KR3TUw7OEOVvrihWrb0tBKUd7a2Q2Hl7Sf1XQOlV\n\
pG+rz8APWbQtQs715w6Y7juwwZWWimDXOG0aLbkmq5QQZSG3JUZNn5ECgYEA2h1L\n\
saKhRtkdzA/C3TCGM50dK7d3mDJP+o9gFTp0N/BuS4Q3QjHIsyyVsyKsj/4nPLDE\n\
2ooXaK1iWQzSdySZOK4u0xckGw/xUlOV+UqiciM7Nqxuja8fBs/IDy2xlJ0ti/Sa\n\
+17qh/6Pb/VYePbPA3a6u4H2Ro+3T5B9e5rpfLkCgYAYU2bWF1/iu0CtdaN8AAyc\n\
pblRPmZVC3ZBtY8yFZTwNB8g+kalzngGo3LzYZPhWuL15HjDL2fcAku9Ji9weKss\n\
09azTwuB//ShzXXhqBWNr5o0ryoAAGNHM6+4byUJ5k+xaUoOsUfbE78R09htznzX\n\
3R/14x3iuIIaMfHwkZ5BAQKBgQDR2BWXFWwsiX6NcSx9Oc4joikKgjzhhKZF3eMH\n\
CXH+z6aNqOqxGMyK9X4hFl8HOfHuBfOeffT/lLBmFFv4nJF9YrdSB5WJI9F870X+\n\
zbt0LEkv1L2YOr+TAhzr3X5YCNBlMjRZW3ww0syVXmp8FpgcMQJ+nA6g3Gv0dIMF\n\
hIjWoQKBgAbevtfsywFMVjzWa8R9FbGFX9i8shltMUrfTLvvmSOIolAy5IkfIeUQ\n\
z2thEaZ+75BmwBpa1V7rvZRi5dVBvaug8l682qdxDjakpyqPgM4073X3q6dNUYcK\n\
zVN9hQGIJhxAIsOTjDIeddK71rC5mfMn/K/MFdc6XRnywNlAQB1q\n\
-----END RSA PRIVATE KEY-----\n";

    /// Base64url RSA modulus matching [`PRIVATE_PEM`].
    pub const JWK_N: &str = "2AQHKy4lzOoachm0nq2icBbbC76rNzFts8j60FjDSypxrBAV2tFq963hQoD0N7Wz4-eqrTMgDOoTokUNBp7V5X8hbW4aJJaDFIvcN04TELaIMJ7MZ-vQOd9yMSBy0ybndx3w0En8MY0BQ5jZuwZhKiZyz42J6fm5zlCsvKWP8YmLpk92yP7l45TKOJhbJnL-biqKzPgydCwQv6-b-QPw4himhd-bWUF2ibwZ4AuNdS9CZGaif_yBivGmoQt5URhftEE84dabDoosevpvM0AEBz5wtiuZmMmRP-ERPFvgmnkH34TZXI2Qg_5XpFw_wYc1JTB0C9YHBny2EI5L3FuLyQ";

    /// Base64url RSA exponent (65537).
    pub const JWK_E: &str = "AQAB";

    /// The `kid` shared by the JWKS document and minted tokens.
    pub const KID: &str = "firefly-test-key";

    /// The JWKS document body the in-process endpoint serves.
    pub fn jwks_document() -> serde_json::Value {
        serde_json::json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": KID,
                "n": JWK_N,
                "e": JWK_E,
            }]
        })
    }
}

/// Mints an RS256 JWT signed with the fixture key, carrying `iss`, `aud`,
/// `exp`, the principal, and roles — the shape the JWKS verifier maps
/// onto an [`Authentication`].
fn mint_token(issuer: &str, audience: &str, sub: &str, roles: &[&str]) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};

    #[derive(Serialize)]
    struct Claims {
        iss: String,
        aud: String,
        sub: String,
        preferred_username: String,
        roles: Vec<String>,
        exp: usize,
    }

    let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(jwks_fixture::KID.to_string());
    let claims = Claims {
        iss: issuer.to_string(),
        aud: audience.to_string(),
        sub: sub.to_string(),
        preferred_username: sub.to_string(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        // Far-future expiry so the test is time-independent.
        exp: 4_102_444_800, // 2100-01-01
    };
    let key = EncodingKey::from_rsa_pem(jwks_fixture::PRIVATE_PEM.as_bytes())
        .expect("fixture PEM parses as an RSA signing key");
    encode(&header, &claims, &key).expect("token signs")
}

/// Spawns the in-process JWKS endpoint on a loopback socket and returns
/// `(jwks_uri, shutdown_addr)`. A real bound socket is needed because the
/// verifier fetches the document over HTTP with `reqwest` — but it is
/// loopback-only and torn down with the test runtime.
async fn spawn_jwks_endpoint() -> String {
    let app = Router::new().route(
        "/.well-known/jwks.json",
        get(|| async { Json(jwks_fixture::jwks_document()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/.well-known/jwks.json")
}

/// The JWKS verifier accepts a token minted in-process against the JWKS
/// document, the bearer layer attaches the resulting [`Authentication`],
/// and the [`FilterChain`] (consulting a [`RoleHierarchy`]) admits an
/// `ADMIN` to a `USER`-gated route while rejecting an unauthenticated
/// request and a wrong-issuer token.
#[tokio::test]
async fn jwks_bearer_gates_filter_chain_route_with_role_hierarchy() {
    use firefly_security::{BearerConfig, BearerLayer, FilterChain, JwksVerifier, RoleHierarchy};

    let issuer = "https://auth.firefly.test";
    let audience = "orders-api";
    let jwks_uri = spawn_jwks_endpoint().await;

    let verifier = JwksVerifier::new(jwks_uri)
        .issuer(issuer)
        .audience(audience)
        .algorithms(vec![jsonwebtoken::Algorithm::RS256]);

    // ADMIN > USER: an ADMIN principal satisfies a USER-gated rule.
    let chain = FilterChain::new()
        .permit("/public")
        .require("/api/", &["USER"])
        .with_role_hierarchy(RoleHierarchy::from_string("ADMIN > USER"));

    let app: Router = Router::new()
        .route(
            "/api/orders",
            get(
                |Extension(auth): Extension<firefly_security::Authentication>| async move {
                    format!("hello {} ({:?})", auth.username, auth.roles)
                },
            ),
        )
        .route("/public", get(|| async { "open" }))
        // Layers run outermost-last: bearer authenticates, then the chain.
        // allow_anonymous lets the missing-token path fall through as the
        // anonymous principal, so the chain's permit("/public") rule serves
        // the public route while its require("/api/") rule still 401s an
        // anonymous caller. An *invalid* token (e.g. wrong issuer) is still
        // rejected at the bearer layer regardless of allow_anonymous.
        .layer(chain.layer())
        .layer(BearerLayer::new(
            BearerConfig::new(verifier).allow_anonymous(true),
        ));

    // --- 2a. An ADMIN token (no explicit USER role) passes the USER gate. ---
    let admin_token = mint_token(issuer, audience, "alice", &["ADMIN"]);
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/orders")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "ADMIN admitted via hierarchy");
    let body = String::from_utf8(body_bytes(res).await).unwrap();
    assert!(body.contains("hello alice"), "handler saw auth: {body}");

    // --- 2b. No token → 401 from the bearer layer. ---
    let res = app
        .clone()
        .oneshot(Request::get("/api/orders").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "missing token");

    // --- 2c. A token from the wrong issuer is rejected (401). ---
    let bad_token = mint_token("https://evil.example", audience, "mallory", &["ADMIN"]);
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/orders")
                .header("authorization", format!("Bearer {bad_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "wrong issuer rejected"
    );

    // --- 2d. The permit rule lets the public route through unauthenticated. ---
    let res = app
        .oneshot(Request::get("/public").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "public route permitted");
}

// ===========================================================================
// Scenario 3 — orchestration: a persisted workflow with a wait-for-signal
//              step driven to completion, plus recovery of a stale run
// ===========================================================================

/// A workflow with a `wait_for_signal` node parks until a concurrent task
/// delivers the signal, completing the run; its [`ExecutionState`] is
/// persisted as it transitions, and a separately-seeded *stale* run is
/// repaired by the [`RecoveryService`] — persistence + signal + recovery
/// in one pass.
#[tokio::test]
async fn workflow_persistence_wait_for_signal_and_recovery() {
    use chrono::{Duration as ChronoDuration, Utc};
    use firefly_orchestration::{
        ExecutionFilter, ExecutionPattern, ExecutionState, ExecutionStatus, MemoryPersistence,
        Node, PersistenceProvider, RecoveryAction, RecoveryService, SignalService, Workflow,
    };

    let persistence: Arc<dyn PersistenceProvider> = Arc::new(MemoryPersistence::new());
    let signals = Arc::new(SignalService::new());
    let correlation_id = "wf-approval-1";

    // Persist the run as RUNNING before it parks on the signal — what a
    // host engine would checkpoint at the wait boundary.
    persistence
        .save(
            ExecutionState::new(correlation_id, "approval", ExecutionPattern::Workflow)
                .with_status(ExecutionStatus::Waiting),
        )
        .await
        .expect("persist waiting state");

    // Build the workflow: submit → wait_for_signal(approved) → finalize.
    let workflow = Workflow::new("approval")
        .node(Node::new("submit", || async { Ok(()) }))
        .node(
            Node::wait_for_signal("approve", &signals, correlation_id, "approved")
                .depends_on(["submit"]),
        )
        .node(Node::new("finalize", || async { Ok(()) }).depends_on(["approve"]));

    // Concurrently deliver the signal once the workflow is parked.
    let deliverer = {
        let signals = Arc::clone(&signals);
        tokio::spawn(async move {
            // Poll until the wait node has registered (no fixed sleep).
            for _ in 0..100 {
                if signals.list_active() == [correlation_id] {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert!(signals.deliver(correlation_id, "approved", serde_json::json!("manager-A")));
        })
    };

    // The workflow resumes on the signal and completes within budget.
    tokio::time::timeout(Duration::from_millis(200), workflow.run())
        .await
        .expect("workflow resumes on signal")
        .expect("workflow completes");
    deliverer.await.expect("deliverer joined");
    assert!(signals.list_active().is_empty(), "no dangling waiters");

    // Checkpoint the terminal state, as the engine would on completion.
    {
        let mut state = persistence
            .load(correlation_id)
            .await
            .expect("load")
            .expect("present");
        state.transition(ExecutionStatus::Completed);
        persistence.save(state).await.expect("persist completion");
    }
    let completed = persistence
        .load(correlation_id)
        .await
        .expect("load")
        .expect("present");
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert!(completed.completed_at.is_some());

    // --- Recovery: a second run was abandoned mid-flight (stale RUNNING).
    let mut stale = ExecutionState::new("wf-stale-2", "approval", ExecutionPattern::Workflow)
        .with_status(ExecutionStatus::Running);
    // Backdate it well past the stale threshold.
    stale.updated_at = Utc::now() - ChronoDuration::hours(2);
    persistence.save(stale).await.expect("persist stale run");

    let recovery = RecoveryService::new(Arc::clone(&persistence))
        .stale_threshold(ChronoDuration::minutes(30))
        .decider(|_state| RecoveryAction::MarkFailed);
    let recovered = recovery.recover_stale().await.expect("recover");
    assert_eq!(recovered, 1, "exactly the stale run was repaired");

    // The completed run is untouched; the stale one is now FAILED.
    assert_eq!(
        persistence
            .load(correlation_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ExecutionStatus::Completed
    );
    assert_eq!(
        persistence
            .load("wf-stale-2")
            .await
            .unwrap()
            .unwrap()
            .status,
        ExecutionStatus::Failed
    );

    // The persistence list view reflects both runs.
    let all = persistence
        .list(ExecutionFilter::all())
        .await
        .expect("list");
    assert_eq!(all.len(), 2);
}

// ===========================================================================
// Scenario 4 — CQRS authorization middleware denying then allowing a
//              command via ExecutionContext, plus EDA cache-invalidation
//              evicting a cached query
// ===========================================================================

/// A command authorizes against the dispatch's [`ExecutionContext`]:
/// without the required tenant it is denied, with it the handler runs.
/// Separately, a cached query is served from the [`QueryCache`] until an
/// EDA event arrives on the broker and the
/// [`EdaCacheInvalidationBridge`] evicts the entry — CQRS authorization +
/// CQRS cache + EDA broker in one flow.
#[tokio::test]
async fn cqrs_authorization_via_context_and_eda_cache_invalidation() {
    use firefly_cqrs::{
        AuthorizationMiddleware, AuthorizationResult, Bus, CqrsError, EdaCacheInvalidationBridge,
        ExecutionContext, Message, QueryCache,
    };
    use firefly_eda::{Event, InMemoryBroker};

    // ---- 4a. Authorization keyed off the ExecutionContext tenant. ----
    #[derive(Clone, Serialize)]
    struct ShipOrder {
        order_id: String,
    }
    impl Message for ShipOrder {
        fn authorize(&self, ctx: Option<&ExecutionContext>) -> AuthorizationResult {
            // Only callers in tenant "acme" may ship.
            let tenant_ok = ctx
                .and_then(|c| c.tenant_id.as_deref())
                .is_some_and(|t| t == "acme");
            if tenant_ok {
                AuthorizationResult::success()
            } else {
                AuthorizationResult::failure("orders", "tenant not permitted to ship")
            }
        }
    }

    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    let shipped = Arc::new(AtomicU32::new(0));
    let count = Arc::clone(&shipped);
    bus.register(move |_cmd: ShipOrder| {
        let count = Arc::clone(&count);
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>("SHIPPED".to_string())
        }
    });

    // Deny: a context in the wrong tenant.
    let wrong = ExecutionContext::builder().with_tenant_id("globex").build();
    let err = bus
        .send_with_context::<ShipOrder, String>(
            ShipOrder {
                order_id: "o-1".into(),
            },
            wrong,
        )
        .await
        .expect_err("denied");
    assert!(matches!(err, CqrsError::Authorization(_)), "got: {err}");
    assert_eq!(err.to_string(), "orders: tenant not permitted to ship");
    assert_eq!(shipped.load(Ordering::SeqCst), 0, "handler never ran");

    // Allow: the right tenant.
    let right = ExecutionContext::builder().with_tenant_id("acme").build();
    let out: String = bus
        .send_with_context(
            ShipOrder {
                order_id: "o-1".into(),
            },
            right,
        )
        .await
        .expect("authorized");
    assert_eq!(out, "SHIPPED");
    assert_eq!(shipped.load(Ordering::SeqCst), 1);

    // ---- 4b. EDA event evicts a cached query result. ----
    #[derive(Clone, Serialize)]
    struct GetOrder {
        order_id: String,
    }
    impl Message for GetOrder {
        fn cache_ttl(&self) -> Option<Duration> {
            // Cache forever (Duration::ZERO) until invalidated.
            Some(Duration::ZERO)
        }
    }

    let cache = QueryCache::new();
    let read_bus = Bus::new();
    read_bus.use_middleware(cache.middleware());

    // The handler counts executions so we can prove cache hits vs misses.
    let reads = Arc::new(AtomicU32::new(0));
    let reads_h = Arc::clone(&reads);
    read_bus.register(move |q: GetOrder| {
        let reads = Arc::clone(&reads_h);
        async move {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(format!("order:{}", q.order_id))
        }
    });

    // First query → miss (handler runs); second → hit (handler skipped).
    let q = GetOrder {
        order_id: "42".into(),
    };
    let v1: String = read_bus.query(q.clone()).await.expect("first read");
    let v2: String = read_bus.query(q.clone()).await.expect("cached read");
    assert_eq!(v1, v2);
    assert_eq!(reads.load(Ordering::SeqCst), 1, "second read was cached");

    // Wire the invalidation bridge to the broker. An "order.updated" event
    // carrying {"order_id":"42"} evicts the "...GetOrder:" family by the
    // registered pattern prefix.
    let bridge = EdaCacheInvalidationBridge::new(cache.clone());
    let key_prefix = std::any::type_name::<GetOrder>();
    // Register a rule whose resolved key is the exact cache-key family
    // prefix the QueryCache stores entries under (<type name>:...).
    bridge.register("order.updated", format!("{key_prefix}:"));
    let broker = InMemoryBroker::new();
    bridge
        .subscribe(&broker, "domain.events")
        .await
        .expect("subscribe bridge");

    broker
        .publish(Event::new(
            "domain.events",
            "order.updated",
            "orders-svc",
            Some(br#"{"order_id":"42"}"#.to_vec()),
        ))
        .await
        .expect("publish invalidation");

    // Next read is a miss again — the bridge evicted the entry.
    let v3: String = read_bus.query(q).await.expect("post-eviction read");
    assert_eq!(v3, "order:42");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "EDA event invalidated the cached query"
    );
}

// ===========================================================================
// Scenario 5 — eventsourcing transactional outbox publishing onto an
//              in-memory eda broker
// ===========================================================================

/// An aggregate raises a domain event, the [`TransactionalOutbox`]
/// enqueues it, and its background relay forwards it through an
/// [`EdaSink`] onto an in-memory EDA broker where a subscriber receives
/// the wrapped [`Event`] with the aggregate headers — eventsourcing →
/// outbox → eda end to end.
#[tokio::test]
async fn eventsourcing_outbox_publishes_onto_eda_broker() {
    use firefly_eda::{handler, Event, InMemoryBroker};
    use firefly_eventsourcing::{AggregateRoot, EdaSink, TransactionalOutbox};

    let broker = Arc::new(InMemoryBroker::new());

    // Subscribe a recorder before the relay publishes. (InMemoryBroker's
    // inherent subscribe is synchronous; the async Subscriber-trait method
    // is what transport adapters override.)
    let received: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_rx = Arc::clone(&received);
    broker
        .subscribe(
            "account.events",
            handler(move |ev: Event| {
                let sink_rx = Arc::clone(&sink_rx);
                async move {
                    sink_rx.lock().unwrap().push(ev);
                    Ok(())
                }
            }),
        )
        .expect("subscribe recorder");

    // Wire the outbox over an EdaSink targeting the broker. Fast poll so
    // delivery happens well within the no-sleep-over-200ms budget.
    let sink = Arc::new(EdaSink::new(
        Arc::clone(&broker) as Arc<dyn firefly_eda::Publisher>,
        "account.events",
        "accounts-svc",
    ));
    let outbox = TransactionalOutbox::new(sink).with_poll_interval(Duration::from_millis(5));

    // Raise an event on an aggregate and enqueue it.
    let mut account = AggregateRoot::new("acc-1", "Account");
    account.raise("AccountOpened", br#"{"owner":"Ada","balance":100}"#);
    let event = account.take_uncommitted().remove(0);
    let record = outbox.enqueue(event).await;

    // Start the relay and poll for delivery (bounded; no fixed long sleep).
    outbox.start().await;
    for _ in 0..100 {
        if record.delivered() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    outbox.stop().await;

    assert!(record.delivered(), "outbox relay delivered the event");
    assert_eq!(record.attempts(), 0, "delivered first try");

    let got = received.lock().unwrap();
    assert_eq!(got.len(), 1, "broker received exactly one event");
    let ev = &got[0];
    assert_eq!(ev.topic, "account.events");
    assert_eq!(ev.event_type, "AccountOpened");
    assert_eq!(ev.source, "accounts-svc");
    assert_eq!(
        ev.payload.as_deref(),
        Some(&br#"{"owner":"Ada","balance":100}"#[..])
    );
    // EdaSink stamps aggregate routing headers.
    assert_eq!(
        ev.headers.get("aggregate_id").map(String::as_str),
        Some("acc-1")
    );
    assert_eq!(
        ev.headers.get("aggregate_type").map(String::as_str),
        Some("Account")
    );
    assert_eq!(ev.headers.get("version").map(String::as_str), Some("1"));
}

// ===========================================================================
// Scenario 6 — eda subscribe_group round-robin + wrap_listener DLQ
// ===========================================================================

/// Two members of a consumer group compete for events (round-robin: each
/// event reaches exactly one member), and a third subscription wraps an
/// always-failing handler with [`wrap_listener`] so an exhausted event is
/// dead-lettered onto a DLQ topic where a recorder observes it with the
/// diagnostic headers — pyfly's competing-consumer + retry/DLQ seams in
/// one broker.
#[tokio::test]
async fn eda_subscribe_group_round_robin_and_wrap_listener_dlq() {
    use firefly_eda::{
        handler, Event, InMemoryBroker, ListenerPolicy, Publisher, HEADER_EXCEPTION,
        HEADER_ORIGINAL_TOPIC,
    };

    let broker = Arc::new(InMemoryBroker::new());

    // ---- 6a. Round-robin across a consumer group. ----
    // (InMemoryBroker's inherent subscribe/subscribe_group are synchronous.)
    let a_hits = Arc::new(AtomicU32::new(0));
    let b_hits = Arc::new(AtomicU32::new(0));
    let a = Arc::clone(&a_hits);
    let b = Arc::clone(&b_hits);
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let a = Arc::clone(&a);
                async move {
                    a.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .expect("group member A");
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let b = Arc::clone(&b);
                async move {
                    b.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .expect("group member B");

    // Four events → exactly one member per event → 2 each (round-robin).
    for i in 0..4 {
        broker
            .publish(Event::new(
                "orders",
                "OrderPlaced",
                "svc",
                Some(format!(r#"{{"n":{i}}}"#).into_bytes()),
            ))
            .await
            .expect("publish to group");
    }
    assert_eq!(
        a_hits.load(Ordering::SeqCst) + b_hits.load(Ordering::SeqCst),
        4,
        "every event delivered exactly once across the group"
    );
    assert_eq!(a_hits.load(Ordering::SeqCst), 2, "round-robin to A");
    assert_eq!(b_hits.load(Ordering::SeqCst), 2, "round-robin to B");

    // ---- 6b. wrap_listener dead-letters an exhausted event. ----
    // A recorder on the DLQ topic.
    let dlq: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let dlq_rx = Arc::clone(&dlq);
    broker
        .subscribe(
            "orders.DLT",
            handler(move |ev: Event| {
                let dlq_rx = Arc::clone(&dlq_rx);
                async move {
                    dlq_rx.lock().unwrap().push(ev);
                    Ok(())
                }
            }),
        )
        .expect("subscribe DLQ recorder");

    // An always-failing handler wrapped with a 1-retry → DLQ policy.
    let inner = handler(|_ev: Event| async {
        Err(firefly_kernel::FireflyError::internal("processing boom"))
    });
    let wrapped = firefly_eda::wrap_listener(
        inner,
        Arc::clone(&broker) as Arc<dyn Publisher>,
        ListenerPolicy::with_retries(1).dead_letter_topic("orders.DLT"),
    );
    broker
        .subscribe("payments", wrapped)
        .expect("subscribe wrapped handler");

    broker
        .publish(
            Event::new("payments", "PaymentRequested", "svc", Some(b"{}".to_vec()))
                .with_header("trace", "t-1"),
        )
        .await
        .expect("publish to failing handler");

    let dead = dlq.lock().unwrap();
    assert_eq!(dead.len(), 1, "exhausted event was dead-lettered");
    let dl = &dead[0];
    assert_eq!(dl.topic, "orders.DLT");
    assert_eq!(
        dl.headers.get(HEADER_ORIGINAL_TOPIC).map(String::as_str),
        Some("payments"),
        "DLQ records the original topic"
    );
    assert!(
        dl.headers.contains_key(HEADER_EXCEPTION),
        "DLQ records the failing exception code"
    );
    // Original headers ride along.
    assert_eq!(dl.headers.get("trace").map(String::as_str), Some("t-1"));
}

// ===========================================================================
// Scenario 7 — notifications DefaultEmailService opt-out suppression +
//              template precedence with a Dummy provider
// ===========================================================================

/// The [`DefaultEmailService`] prunes opted-out recipients (suppressing
/// the send entirely when none remain), renders a local template over the
/// provider-native one when a [`TemplateEngine`] is injected, and folds
/// everything onto a [`DummyEmailProvider`] — opt-out + template
/// precedence + provider in one service.
#[tokio::test]
async fn notifications_email_opt_out_and_template_precedence() {
    use firefly_notifications::{
        DefaultEmailService, DeliveryStatus, DummyEmailProvider, EmailMessage, EmailService,
        InMemoryPreferenceService, MiniJinjaTemplateEngine,
    };

    let provider = Arc::new(DummyEmailProvider::new());
    let prefs = Arc::new(InMemoryPreferenceService::new());
    // Alice opted out of email; Bob did not.
    prefs.opt_out("alice@example.com", "email");

    let engine = Arc::new(MiniJinjaTemplateEngine::new([(
        "welcome".to_string(),
        "<h1>Hi {{ name }}</h1>".to_string(),
    )]));

    let service = DefaultEmailService::new(Arc::clone(&provider) as _)
        .with_preference_service(Arc::clone(&prefs) as _)
        .with_template_engine(Arc::clone(&engine) as _);

    // --- 7a. All recipients opted out → suppressed, provider never called. ---
    let mut only_alice = EmailMessage::new();
    only_alice.to = vec!["alice@example.com".into()];
    only_alice.subject = "hello".into();
    only_alice.body_html = Some("<p>hi</p>".into());
    let result = service.send(only_alice).await;
    assert_eq!(result.status, DeliveryStatus::Suppressed);
    assert!(
        provider.sent().is_empty(),
        "provider not called when suppressed"
    );

    // --- 7b. Mixed recipients: Alice pruned, Bob kept; local template wins. ---
    let mut msg = EmailMessage::new();
    msg.to = vec!["alice@example.com".into(), "bob@example.com".into()];
    msg.subject = "welcome".into();
    msg.template_id = Some("welcome".into());
    msg.template_data = HashMap::from([("name".to_string(), serde_json::json!("Bob"))]);
    let result = service.send(msg).await;
    assert_eq!(result.status, DeliveryStatus::Sent);

    let sent = provider.sent();
    assert_eq!(sent.len(), 1, "exactly one provider delivery");
    let delivered = &sent[0];
    // Alice was pruned; only Bob remains.
    assert_eq!(delivered.to, vec!["bob@example.com".to_string()]);
    // Local template rendered into body_html; provider-native template
    // routing was cleared (local precedence).
    assert_eq!(delivered.body_html.as_deref(), Some("<h1>Hi Bob</h1>"));
    assert!(
        delivered.template_id.is_none(),
        "local rendering cleared the provider-native template id"
    );
    assert!(delivered.template_data.is_empty());
}

// ===========================================================================
// Scenario 8 — config placeholder resolution + reload + property-source
//              masking end to end
// ===========================================================================

/// A layered config resolves `${...}` placeholders (with environment and
/// default fallbacks), reloads a changed override into a fresh snapshot,
/// and exposes its property sources with sensitive values masked — pyfly's
/// `Config` placeholder + reload + `property_sources()` seam end to end.
#[tokio::test]
async fn config_placeholder_reload_and_property_source_masking() {
    use firefly_config::{
        mask::MASK, resolve_placeholders, FlagSource, Layered, ReloadableConfig, Source,
        StaticSource,
    };
    use serde::Deserialize;

    // ---- 8a. Placeholder resolution: intra-config ref + default fallback. ----
    let base = HashMap::from([
        ("app.name".to_string(), "orders".to_string()),
        (
            "app.greeting".to_string(),
            "service ${app.name}".to_string(),
        ),
        (
            "app.timeout".to_string(),
            "${APP_TIMEOUT_UNSET:30s}".to_string(),
        ),
    ]);
    let resolved = resolve_placeholders(&base).expect("placeholders resolve");
    assert_eq!(resolved["app.greeting"], "service orders");
    assert_eq!(
        resolved["app.timeout"], "30s",
        "missing env falls back to the default"
    );

    // ---- 8b. Reload swaps the snapshot when a source changes. ----
    #[derive(Debug, Deserialize)]
    struct AppCfg {
        feature: String,
    }
    let flags = FlagSource::new();
    flags.set("feature", "alpha");
    let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
    let cfg: ReloadableConfig<AppCfg> = ReloadableConfig::load(sources).expect("load config");
    assert_eq!(cfg.get().feature, "alpha");

    flags.set("feature", "beta");
    let changed = cfg.reload().expect("reload");
    assert!(
        changed.iter().any(|k| k == "feature"),
        "reload reports the changed key: {changed:?}"
    );
    assert_eq!(cfg.get().feature, "beta", "snapshot swapped");

    // ---- 8c. Property sources mask sensitive values, highest precedence first. ----
    let public = StaticSource::new(
        "defaults",
        HashMap::from([
            ("firefly.web.port".to_string(), "8080".to_string()),
            (
                "firefly.security.jwt.secret".to_string(),
                "super-secret-value".to_string(),
            ),
            ("db.password".to_string(), "hunter2".to_string()),
        ]),
    );
    let layered = Layered::new(vec![Box::new(public)]);
    let views = layered.property_sources().expect("property sources");
    // Find the "defaults" source view.
    let defaults = views
        .iter()
        .find(|v| v.name == "defaults")
        .expect("defaults source present");
    // Non-sensitive value passes through; secrets are masked.
    assert_eq!(defaults.properties["firefly.web.port"].value, "8080");
    assert_eq!(
        defaults.properties["firefly.security.jwt.secret"].value, MASK,
        "jwt secret masked"
    );
    assert_eq!(
        defaults.properties["db.password"].value, MASK,
        "db password masked"
    );
    // Origin attribution is the source name.
    assert_eq!(defaults.properties["firefly.web.port"].origin, "defaults");
}
