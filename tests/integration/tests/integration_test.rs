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

//! Cross-module integration tests proving the framework composes
//! end-to-end: a service built on starter-core handles a command, emits
//! a callback signed with HMAC, the receiver acks, the audit trail
//! records the attempt; in parallel, a webhook ingestion endpoint
//! validates the payload, runs the processing pipeline, and surfaces a
//! result — the Rust port of the Go module's `integration_test.go`.
//!
//! Where the Go suite used `httptest.NewServer` for the callback
//! receiver, this port binds a real `axum` server to `127.0.0.1:0`;
//! pure in-process HTTP surfaces (webhook ingestion, the actuator) are
//! driven through `tower::ServiceExt::oneshot` instead.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use axum::Router;
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use serde::Serialize;
use sha2::Sha256;
use tower::ServiceExt;

use firefly_callbacks::{
    CallbackEvent, Dispatcher, DispatcherConfig, HmacDispatcher, MemoryStore, Store, Target,
    HEADER_EVENT, HEADER_EVENT_ID, HEADER_SIGNATURE, HEADER_TIMESTAMP,
};
use firefly_cqrs::{CqrsError, Message};
use firefly_kernel::{FireflyError, HEADER_CORRELATION_ID};
use firefly_orchestration::{Saga, SagaStatus, Step};
use firefly_starter_core::{Core, CoreConfig, HealthStatus};
use firefly_webhooks::{
    HmacValidator, Inbound, MemoryDlq, Pipeline, Processor, Validator, WebhookError,
};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// `CreateOrder` is a sample command exercised by the integration test —
/// the Go `CreateOrder` struct whose `Validate` made it a
/// `cqrs.Validatable`.
#[derive(Clone, Serialize)]
struct CreateOrder {
    customer: String,
    total: f64,
}

impl Message for CreateOrder {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.total <= 0.0 {
            // Go: kernel.NewValidation("total must be > 0"); the Rust bus
            // speaks CqrsError, whose Validation variant displays the
            // message verbatim, exactly like Go's middleware returning
            // the Validate() error unchanged.
            return Err(CqrsError::validation("total must be > 0"));
        }
        Ok(())
    }
}

/// `OrderPlaced` is the result type.
#[derive(Clone, Debug, PartialEq)]
struct OrderPlaced {
    id: String,
}

/// A read-side query, dispatched through the same bus (Rust-specific
/// coverage of the command+query roundtrip through starter-core wiring).
#[derive(Clone, Serialize)]
struct GetOrder {
    id: String,
}

impl Message for GetOrder {}

#[derive(Clone, Debug, PartialEq)]
struct OrderView {
    id: String,
    customer: String,
}

/// State recorded by the local callback receiver — the analog of the Go
/// test's `hits` counter and `lastSig` capture, extended to keep the
/// full header map and body so the webhooks validators can re-verify
/// the delivery.
struct Receiver {
    status: StatusCode,
    hits: AtomicU32,
    last_headers: Mutex<Option<HeaderMap>>,
    last_body: Mutex<Vec<u8>>,
}

impl Receiver {
    fn new(status: StatusCode) -> Self {
        Self {
            status,
            hits: AtomicU32::new(0),
            last_headers: Mutex::new(None),
            last_body: Mutex::new(Vec::new()),
        }
    }

    fn hits(&self) -> u32 {
        self.hits.load(Ordering::SeqCst)
    }

    fn last_headers(&self) -> HeaderMap {
        self.last_headers
            .lock()
            .unwrap()
            .clone()
            .expect("receiver captured headers")
    }

    fn last_body(&self) -> Vec<u8> {
        self.last_body.lock().unwrap().clone()
    }

    fn header(&self, name: &str) -> String {
        self.last_headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }
}

async fn record(State(state): State<Arc<Receiver>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    state.hits.fetch_add(1, Ordering::SeqCst);
    *state.last_headers.lock().unwrap() = Some(headers);
    *state.last_body.lock().unwrap() = body.to_vec();
    state.status
}

/// Boots a real loopback HTTP server on port 0 — the Rust analog of
/// Go's `httptest.NewServer` — answering every POST with the configured
/// status while recording hits, headers, and body.
async fn spawn_receiver(status: StatusCode) -> (String, Arc<Receiver>) {
    let state = Arc::new(Receiver::new(status));
    let app = Router::new()
        .route("/", post(record))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        // The task is dropped with the test runtime; serve errors at
        // that point are irrelevant.
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state)
}

/// Builds the dispatcher the Go test configures:
/// `cbcore.NewDispatcher(store, Config{InitialDelay: 1ms, MaxAttempts: 2})`.
fn fast_dispatcher(store: Arc<MemoryStore>) -> Arc<HmacDispatcher> {
    Arc::new(HmacDispatcher::new(
        store,
        DispatcherConfig {
            initial_delay: Duration::from_millis(1),
            max_attempts: 2,
            ..DispatcherConfig::default()
        },
    ))
}

/// `sha256=<hmac-hex>` over `body` keyed on `secret` — the signature
/// shape every Firefly runtime agrees on (the Go test computed it
/// inline with crypto/hmac).
fn sign_sha256(secret: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

async fn body_bytes(res: axum::response::Response) -> Vec<u8> {
    res.into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

async fn body_json(res: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(&body_bytes(res).await).expect("json body")
}

// ---------------------------------------------------------------------------
// Ports of the Go integration suite
// ---------------------------------------------------------------------------

/// Go: `TestEndToEndCommandToCallback` — proves: starter-core wires the
/// bus, validation middleware rejects bad commands, accepted commands
/// fire a callback that arrives signed at the receiver, and the audit
/// log is written. Strengthened over Go: the captured signature is
/// re-verified with the webhooks crate's `HmacValidator`, proving the
/// outbound (callbacks) and inbound (webhooks) wire formats agree
/// byte-for-byte.
#[tokio::test]
async fn end_to_end_command_to_callback() {
    // 1. Receiver — a real loopback server pretending to be a
    //    customer's listener. It records the signature header so we can
    //    prove HMAC signing flowed through.
    let (url, receiver) = spawn_receiver(StatusCode::OK).await;

    // 2. Wire the framework: in-memory CQRS bus + in-memory callback
    //    store + the starter-core health composite (its default cache
    //    indicator plays the role of the Go test's manual
    //    `app.Health.Add(IndicatorFunc{"cache", …})`).
    let core = Core::new(CoreConfig {
        app_name: "integration".into(),
        ..CoreConfig::default()
    });
    let store = Arc::new(MemoryStore::new());
    let dispatcher = fast_dispatcher(Arc::clone(&store));
    store
        .upsert_target(Target {
            id: "customers".into(),
            url,
            secret: "shared-secret".into(),
            event_types: vec!["order.placed".into()],
            active: true,
            ..Target::default()
        })
        .await
        .expect("upsert target");

    // 3. Register the command handler. On success it emits a callback.
    let emit = Arc::clone(&dispatcher);
    core.bus.register(move |cmd: CreateOrder| {
        let emit = Arc::clone(&emit);
        async move {
            let out = OrderPlaced {
                id: "ord_42".into(),
            };
            let payload = serde_json::to_vec(&serde_json::json!({
                "id": out.id,
                "customer": cmd.customer,
            }))
            .expect("payload encodes");
            // Best-effort emission, like the Go test's `_ = Dispatch(…)`.
            let _ = emit
                .dispatch(CallbackEvent {
                    id: "evt_1".into(),
                    event_type: "order.placed".into(),
                    payload,
                    correlation_id: "corr-int".into(),
                    ..CallbackEvent::default()
                })
                .await;
            Ok::<_, CqrsError>(out)
        }
    });

    // 4. Validation middleware (pre-installed by starter-core) rejects
    //    an invalid command without invoking the handler.
    let err = core
        .bus
        .send::<CreateOrder, OrderPlaced>(CreateOrder {
            customer: String::new(),
            total: 0.0,
        })
        .await
        .expect_err("expected validation rejection");
    assert!(matches!(err, CqrsError::Validation(_)), "got: {err}");
    assert_eq!(err.to_string(), "total must be > 0");
    assert_eq!(receiver.hits(), 0, "validation should not call handler");

    // 5. Happy path: command succeeds, callback fires, signature is HMAC.
    let out: OrderPlaced = core
        .bus
        .send(CreateOrder {
            customer: "alice".into(),
            total: 10.0,
        })
        .await
        .expect("command succeeds");
    assert_eq!(out.id, "ord_42");
    assert_eq!(receiver.hits(), 1, "receiver hits");

    let sig = receiver.header(HEADER_SIGNATURE);
    assert!(
        sig.len() > "sha256=".len() && sig.starts_with("sha256="),
        "signature header missing: {sig:?}"
    );

    // The wire format is the cross-port contract: body == the JSON
    // payload with Go's sorted-map key order, the event headers are
    // stamped, and the correlation id flows through.
    let body = receiver.last_body();
    assert_eq!(body, br#"{"customer":"alice","id":"ord_42"}"#);
    assert_eq!(receiver.header(HEADER_EVENT), "order.placed");
    assert_eq!(receiver.header(HEADER_EVENT_ID), "evt_1");
    assert_eq!(receiver.header(HEADER_CORRELATION_ID), "corr-int");
    receiver
        .header(HEADER_TIMESTAMP)
        .parse::<i64>()
        .expect("timestamp is unix seconds");

    // Cross-module proof: the webhooks validator accepts the callbacks
    // dispatcher's signature unchanged.
    let validator = HmacValidator {
        provider_name: "firefly".into(),
        secret: b"shared-secret".to_vec(),
        header: HEADER_SIGNATURE.to_owned(),
        hex_encoded: true,
    };
    validator
        .verify(&receiver.last_headers(), &body)
        .expect("webhooks HmacValidator verifies the callbacks delivery");

    // 6. Audit record was written.
    let attempts = store.list_attempts("evt_1").await.expect("list attempts");
    assert_eq!(attempts.len(), 1, "audit: {attempts:?}");
    assert_eq!(attempts[0].status, 200, "audit: {attempts:?}");

    // The starter-core health composite (observability glue) is UP,
    // including the default cache indicator.
    let (overall, results) = core.health.check_all().await;
    assert_eq!(overall, HealthStatus::Up);
    assert_eq!(results["cache"].status, HealthStatus::Up);
}

/// Counts pipeline invocations and captures the last event — the Go
/// test's `processorFunc` adapter.
struct CountingProcessor {
    provider: String,
    processed: Arc<AtomicU32>,
    seen: Arc<Mutex<Option<Inbound>>>,
}

#[async_trait]
impl Processor for CountingProcessor {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn process(&self, ev: &Inbound) -> Result<(), WebhookError> {
        self.processed.fetch_add(1, Ordering::SeqCst);
        *self.seen.lock().unwrap() = Some(ev.clone());
        Ok(())
    }
}

/// Go: `TestWebhookIngestionRoundTrip` — proves: a GitHub-style
/// HMAC-signed body arrives, the validator accepts it, the pipeline
/// runs the processor, and a bad signature returns 401 without invoking
/// the processor.
#[tokio::test]
async fn webhook_ingestion_round_trip() {
    let secret = b"rolling-secret";

    let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
    pipeline.register_validator(HmacValidator::new("github", secret.to_vec()));

    let processed = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(None));
    pipeline.register_processor(CountingProcessor {
        provider: "github".into(),
        processed: Arc::clone(&processed),
        seen: Arc::clone(&seen),
    });

    let app = firefly_webhooks::web::router(Arc::clone(&pipeline));
    let body: &[u8] = br#"{"hello":"world"}"#;

    // Bad signature → 401, processor never runs.
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/webhooks/github")
                .header("X-Signature", "sha256=deadbeef")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(processed.load(Ordering::SeqCst), 0);

    // Happy path: correctly signed body → 202 Accepted, processor runs.
    let sig = sign_sha256(secret, body);
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/webhooks/github")
                .header("X-Signature", sig)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "happy path status");
    assert_eq!(processed.load(Ordering::SeqCst), 1, "processor calls");

    let ev = seen.lock().unwrap().clone().expect("processor saw event");
    assert_eq!(ev.provider, "github", "processor saw bad event: {ev:?}");
    assert_eq!(ev.payload, body, "processor saw bad event: {ev:?}");
}

/// Go: `TestSagaRollsBackOnFailure` — proves orchestration's
/// compensation path runs when one step fails downstream — exercised
/// here against step closures that mutate test-local state.
#[tokio::test]
async fn saga_rolls_back_on_failure() {
    let rolled_back: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mk = |name: &str, fail: bool| {
        let rolled = Arc::clone(&rolled_back);
        let step_name = name.to_owned();
        Step::new(name, move || async move {
            if fail {
                // Go: kernel.NewInternal("boom") — the kernel error
                // crosses the orchestration boundary as the BoxError.
                Err(FireflyError::internal("boom").into())
            } else {
                Ok(())
            }
        })
        .with_compensation(move || {
            let rolled = Arc::clone(&rolled);
            let step_name = step_name.clone();
            async move {
                rolled.lock().unwrap().push(step_name);
                Ok(())
            }
        })
    };

    let saga = Saga::new("checkout")
        .step(mk("reserve", false))
        .step(mk("charge", false))
        .step(mk("ship", true));

    let failure = saga.run().await.expect_err("expected saga to fail");
    assert_eq!(
        failure.outcome().status,
        SagaStatus::Compensated,
        "status: {}",
        failure.outcome().status
    );
    // Rollback in reverse: charge, then reserve.
    assert_eq!(*rolled_back.lock().unwrap(), ["charge", "reserve"]);
    assert_eq!(failure.outcome().steps_rolled, ["charge", "reserve"]);
}

// ---------------------------------------------------------------------------
// Rust-specific cross-module seams
// ---------------------------------------------------------------------------

/// The saga happy path: every step executes, nothing compensates, and
/// the outcome carries the completed status — the forward half of the
/// roundtrip whose rollback half is `saga_rolls_back_on_failure`.
#[tokio::test]
async fn saga_happy_path_completes() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rolled_back: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mk = |name: &str| {
        let executed = Arc::clone(&executed);
        let rolled = Arc::clone(&rolled_back);
        let step_name = name.to_owned();
        let rollback_name = name.to_owned();
        Step::new(name, move || {
            let executed = Arc::clone(&executed);
            let step_name = step_name.clone();
            async move {
                executed.lock().unwrap().push(step_name);
                Ok(())
            }
        })
        .with_compensation(move || {
            let rolled = Arc::clone(&rolled);
            let rollback_name = rollback_name.clone();
            async move {
                rolled.lock().unwrap().push(rollback_name);
                Ok(())
            }
        })
    };

    let saga = Saga::new("checkout")
        .step(mk("reserve"))
        .step(mk("charge"))
        .step(mk("ship"));

    let outcome = saga.run().await.expect("saga completes");
    assert_eq!(outcome.status, SagaStatus::Completed);
    assert_eq!(outcome.steps_executed, ["reserve", "charge", "ship"]);
    assert_eq!(*executed.lock().unwrap(), ["reserve", "charge", "ship"]);
    assert!(outcome.steps_rolled.is_empty());
    assert!(rolled_back.lock().unwrap().is_empty());
    assert!(outcome.error.is_none());
}

/// The CQRS command+query roundtrip through starter-core wiring: the
/// pre-wired bus dispatches a command, then a query reads the result
/// back, and the pre-installed validation middleware guards both paths.
#[tokio::test]
async fn cqrs_command_and_query_roundtrip_through_starter_core() {
    let core = Core::new(CoreConfig {
        app_name: "integration".into(),
        ..CoreConfig::default()
    });

    // Write side: the command mutates a shared in-memory view.
    let orders: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let write = Arc::clone(&orders);
    core.bus.register(move |cmd: CreateOrder| {
        let write = Arc::clone(&write);
        async move {
            let id = format!("ord_{}", write.lock().unwrap().len() + 1);
            write.lock().unwrap().push((id.clone(), cmd.customer));
            Ok::<_, CqrsError>(OrderPlaced { id })
        }
    });

    // Read side: the query projects the view.
    let read = Arc::clone(&orders);
    core.bus.register(move |q: GetOrder| {
        let read = Arc::clone(&read);
        async move {
            read.lock()
                .unwrap()
                .iter()
                .find(|(id, _)| *id == q.id)
                .map(|(id, customer)| OrderView {
                    id: id.clone(),
                    customer: customer.clone(),
                })
                .ok_or_else(|| CqrsError::handler(format!("order {} not found", q.id)))
        }
    });

    // Command → query roundtrip.
    let placed: OrderPlaced = core
        .bus
        .send(CreateOrder {
            customer: "alice".into(),
            total: 10.0,
        })
        .await
        .expect("command succeeds");
    let view: OrderView = core
        .bus
        .query(GetOrder {
            id: placed.id.clone(),
        })
        .await
        .expect("query succeeds");
    assert_eq!(
        view,
        OrderView {
            id: placed.id,
            customer: "alice".into()
        }
    );

    // The starter-core validation middleware guards the write path…
    let err = core
        .bus
        .send::<CreateOrder, OrderPlaced>(CreateOrder {
            customer: "mallory".into(),
            total: -1.0,
        })
        .await
        .expect_err("invalid command rejected");
    assert!(matches!(err, CqrsError::Validation(_)));
    assert_eq!(orders.lock().unwrap().len(), 1, "handler never ran");

    // …and a domain failure surfaces verbatim on the read path.
    let err = core
        .bus
        .query::<GetOrder, OrderView>(GetOrder {
            id: "ord_999".into(),
        })
        .await
        .expect_err("missing order");
    assert_eq!(err.to_string(), "order ord_999 not found");
}

/// The observability health composite feeds starter-core's actuator
/// surface: a degraded observability indicator (bridged through
/// `Core::add_observability_indicator`) rolls the composite to
/// DEGRADED while the wired cache probe stays UP — all observed
/// through `GET /actuator/health` in-process.
#[tokio::test]
async fn health_composite_over_starter_core() {
    let core = Core::new(CoreConfig {
        app_name: "integration".into(),
        ..CoreConfig::default()
    });

    // The Go test registered an observability IndicatorFunc probing the
    // wired cache; the Rust starter wires that probe by default, so
    // here the bridge carries a service-specific indicator instead.
    let queue_depth = Arc::new(AtomicU32::new(3));
    let depth = Arc::clone(&queue_depth);
    core.add_observability_indicator(firefly_observability::IndicatorFn::new(
        "queue",
        move || {
            let depth = Arc::clone(&depth);
            async move {
                firefly_observability::HealthResult::degraded("cold start")
                    .with_detail("depth", depth.load(Ordering::SeqCst))
            }
        },
    ));

    let admin = core.actuator_router(Vec::new());
    let res = admin
        .oneshot(
            Request::get("/actuator/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // DEGRADED still answers 200, like every other port.
    assert_eq!(res.status(), StatusCode::OK);
    let health = body_json(res).await;
    assert_eq!(health["status"], "DEGRADED");
    assert_eq!(health["details"]["queue"]["status"], "DEGRADED");
    assert_eq!(health["details"]["queue"]["message"], "cold start");
    assert_eq!(health["details"]["queue"]["details"]["depth"], 3);
    assert_eq!(health["details"]["cache"]["status"], "UP");
}

/// The correlation id seam: an id arriving on an HTTP request flows
/// through starter-core's middleware into the kernel task-local scope,
/// is read back by the handler, rides the callback dispatch, and lands
/// on the receiver as `X-Correlation-Id` — kernel → web → callbacks →
/// HTTP, one id end to end.
#[tokio::test]
async fn correlation_id_flows_from_http_request_to_callback_delivery() {
    let (url, receiver) = spawn_receiver(StatusCode::OK).await;

    let store = Arc::new(MemoryStore::new());
    let dispatcher = fast_dispatcher(Arc::clone(&store));
    store
        .upsert_target(Target {
            id: "customers".into(),
            url,
            secret: "shared-secret".into(),
            active: true,
            ..Target::default()
        })
        .await
        .expect("upsert target");

    let core = Core::new(CoreConfig {
        app_name: "integration".into(),
        ..CoreConfig::default()
    });
    let emit = Arc::clone(&dispatcher);
    let api = core.apply_middleware(Router::new().route(
        "/orders",
        post(move || {
            let emit = Arc::clone(&emit);
            async move {
                // CorrelationLayer (installed by apply_middleware) put
                // the request's id into the kernel task-local scope.
                let corr = firefly_kernel::correlation_id().unwrap_or_default();
                let _ = emit
                    .dispatch(CallbackEvent {
                        id: "evt_http".into(),
                        event_type: "order.placed".into(),
                        payload: br#"{"id":"ord_7"}"#.to_vec(),
                        correlation_id: corr,
                        ..CallbackEvent::default()
                    })
                    .await;
                StatusCode::CREATED
            }
        }),
    ));

    let res = api
        .oneshot(
            Request::post("/orders")
                .header(HEADER_CORRELATION_ID, "corr-http-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    // The middleware echoes the id on the HTTP response…
    assert_eq!(
        res.headers().get(HEADER_CORRELATION_ID).unwrap(),
        "corr-http-1"
    );
    // …and the same id arrived on the outbound callback delivery.
    assert_eq!(receiver.hits(), 1);
    assert_eq!(receiver.header(HEADER_CORRELATION_ID), "corr-http-1");
}

/// A processor failure dead-letters the event and surfaces 500 from the
/// ingestion endpoint — webhooks web + core + DLQ in one pass.
#[tokio::test]
async fn webhook_processor_failure_dead_letters_and_returns_500() {
    struct FailingProcessor;

    #[async_trait]
    impl Processor for FailingProcessor {
        fn provider(&self) -> &str {
            "github"
        }

        async fn process(&self, _ev: &Inbound) -> Result<(), WebhookError> {
            Err(WebhookError::processor("kaboom"))
        }
    }

    let secret = b"rolling-secret";
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Arc::new(Pipeline::new(
        Arc::clone(&dlq) as Arc<dyn firefly_webhooks::Dlq>
    ));
    pipeline.register_validator(HmacValidator::new("github", secret.to_vec()));
    pipeline.register_processor(FailingProcessor);

    let app = firefly_webhooks::web::router(pipeline);
    let body: &[u8] = br#"{"hello":"world"}"#;
    let res = app
        .oneshot(
            Request::post("/api/webhooks/github")
                .header("X-Signature", sign_sha256(secret, body))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body_bytes(res).await, b"kaboom\n");

    let entries = dlq.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].err, "kaboom");
    assert_eq!(entries[0].event.provider, "github");
    assert_eq!(entries[0].event.payload, body);
}

/// The audit trail records every delivery attempt: a receiver that
/// always answers 500 consumes the full attempt budget, and the store
/// holds one ordered row per try.
#[tokio::test]
async fn callback_retry_audit_trail_records_every_attempt() {
    let (url, receiver) = spawn_receiver(StatusCode::INTERNAL_SERVER_ERROR).await;

    let store = Arc::new(MemoryStore::new());
    let dispatcher = fast_dispatcher(Arc::clone(&store));
    store
        .upsert_target(Target {
            id: "customers".into(),
            url,
            secret: "shared-secret".into(),
            event_types: vec!["order.placed".into()],
            active: true,
            ..Target::default()
        })
        .await
        .expect("upsert target");

    // Dispatch swallows per-target delivery failures (best-effort fan
    // out, exactly like Go), so the call itself succeeds…
    dispatcher
        .dispatch(CallbackEvent {
            id: "evt_9".into(),
            event_type: "order.placed".into(),
            payload: br#"{"id":"ord_9"}"#.to_vec(),
            ..CallbackEvent::default()
        })
        .await
        .expect("dispatch is best-effort per target");

    // …but the receiver was retried to the attempt budget and the audit
    // trail shows every try in order.
    assert_eq!(receiver.hits(), 2);
    let attempts = store.list_attempts("evt_9").await.expect("list attempts");
    assert_eq!(attempts.len(), 2, "audit: {attempts:?}");
    for (i, attempt) in attempts.iter().enumerate() {
        assert_eq!(attempt.status, 500);
        assert_eq!(attempt.attempt, (i + 1) as u32);
        assert_eq!(attempt.event_id, "evt_9");
        assert_eq!(attempt.target_id, "customers");
    }
}
