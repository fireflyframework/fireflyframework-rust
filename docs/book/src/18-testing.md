# Testing

Every chapter so far has shown Lumen's listings *and* the tests that keep them
honest — that is the whole point of the book: the prose is verified against a
crate that compiles and passes its suite. This chapter steps back and looks at
the test strategy as a whole, the way you would design it for your own service.

By the end of this chapter you will know how Lumen tests at three levels — pure
unit tests with no I/O, in-process HTTP tests that drive the *real* router
through `tower::oneshot`, and the `firefly-testkit` helpers (`TestClient`,
`Slice`, `assert_event_published`) that make all of it terse — and how the same
crate scales up to real-infrastructure integration tests when you need a live
Postgres or Kafka. Lumen's gate is **34 unit tests + 7 HTTP tests + 1 doctest**,
all hermetic; the streaming feature adds 3 more.

> **Spring parity.** The three tiers mirror Spring's: `@Test` unit tests,
> `@WebMvcTest` / `WebTestClient` slice tests, and `@SpringBootTest` +
> Testcontainers integration tests. `TestClient` is the Rust spelling of
> `WebTestClient` / pyfly's `PyFlyTestClient`; `Slice` is a `@…Test` slice; the
> event assertions match pyfly's `assert_event_published`.

## The in-process testing model

Lumen's default stack is entirely in-memory — `MemoryEventStore`,
`InMemoryBroker`, a `Mutex<HashMap>` read model — so almost every test runs as a
plain `#[tokio::test]` with **no socket and no external service**. The HTTP tests
do not bind a port; they hand a `Request` to the router and `await` the
`Response`. That is fast, deterministic, and CI-friendly.

One caveat is worth stating up front, because Lumen's tests are built around it.
Lumen's free-function command handlers and its read-model projection publish
their collaborators through process-global `OnceLock`s (the declarative-macro
pattern from Chapter 9). So the *first* `build_router()` in a test binary wires
the shared ledger that every later test in that binary then drives. The tests
cope by giving each wallet a fresh owner and a server-assigned id, so there is no
cross-test interference even though the ledger is shared.

## Unit tests with no infrastructure

The domain and the value object are pure, so their tests need nothing. Lumen's
`money.rs` and `domain.rs` assert invariants directly — exact-cents arithmetic,
positive amounts, sufficient funds, owner required. The CQRS layer is just as
direct: dispatch a command through a real `Bus` and assert the result. This is
the heart of `commands.rs`'s test module:

```rust,ignore
#[tokio::test]
async fn handlers_dispatch_through_the_bus() {
    let ledger = Ledger::new(
        Arc::new(MemoryEventStore::new()),
        Arc::new(InMemoryBroker::new()),
    );
    bind(ledger, Arc::new(ReadModel::default()));
    let bus = Bus::new();
    // Validation middleware enforces the `#[firefly(validate)]` checks.
    bus.use_middleware(firefly::cqrs::ValidationMiddleware::new());
    register(&bus);

    let opened: WalletView = bus
        .send(OpenWallet { owner: "alice".into(), opening_balance: 100 })
        .await
        .unwrap();
    assert_eq!(opened.balance, 100);

    let after: WalletView = bus
        .send(Deposit { wallet_id: opened.id.clone(), amount: 50 })
        .await
        .unwrap();
    assert_eq!(after.balance, 150);
}
```

Validation is tested without ever touching HTTP — call `.validate()` on the
command directly, because `#[derive(Command)]` generated it from the
`#[firefly(validate)]` fields:

```rust,ignore
#[test]
fn open_wallet_validates_owner() {
    assert!(OpenWallet::default().validate().is_err());      // empty owner
    assert!(OpenWallet { owner: "alice".into(), opening_balance: 0 }.validate().is_ok());
}
```

Security (`security.rs`), the saga (`transfer.rs`), and the scheduled task
(`housekeeping.rs`) each carry their own `#[cfg(test)] mod tests` in the same
spirit — mint-then-verify a token, run the saga happy path and the compensation
path, register the heartbeat and assert it ticks.

## In-process HTTP tests with `tower::oneshot`

The end-to-end suite lives in `tests/http.rs` and drives the **fully-wired**
`build_router()` — the real `#[rest_controller]` routes, the CQRS handlers, the
event-sourced ledger, the read-model projection, the transfer saga, *and* the
JWT/RBAC enforcement from Chapter 14. No mocks: every layer is the production
layer, just over in-memory infrastructure.

The pattern is `Router` + `tower::ServiceExt::oneshot`. Lumen wraps it in two
tiny helpers so each test reads as one HTTP round-trip:

```rust,ignore
use http_body_util::BodyExt;
use tower::ServiceExt;

fn post(path: &str, body: serde_json::Value, auth: bool) -> Request<Body> {
    let mut b = Request::post(path).header("content-type", "application/json");
    if auth {
        b = b.header("authorization", bearer()); // Bearer <minted CUSTOMER token>
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(res: Response) -> T {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
```

A test then opens a wallet and asserts the projected read came back through CQRS:

```rust,ignore
#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    let opened = open_wallet("alice", 1_000).await;   // POST /api/v1/wallets, asserts 201
    assert_eq!(opened.balance, 1_000);

    // GET dispatches the #[query_handler]; the projection has already folded
    // the WalletOpened event into the read model.
    let res = build_router()
        .await
        .oneshot(get(&format!("/api/v1/wallets/{}", opened.id)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let fetched: WalletView = body_json(res).await;
    assert_eq!(fetched.balance, 1_000);
}
```

The same file proves the saga (`transfer_saga_happy_path_moves_funds_between_wallets`),
the compensation path (`transfer_saga_overdraft_compensates_and_is_422`), and the
problem-rendering for the three failure modes — a missing token is a 401, an
empty owner is a 422, an unknown id is a 404 — each asserting the
`application/problem+json` content type. That single suite is the proof that the
whole stack composes.

## The testkit: `TestClient`, `Slice`, event assertions

`firefly-testkit` packages the boilerplate above into reusable helpers. Lumen's
own tests use the raw `tower::oneshot` form to show the mechanism with no magic,
but in your service the testkit makes the same tests far shorter. Three pieces
matter most.

### TestClient — an in-process HTTP client (feature `web`)

`TestClient::new(router)` wraps any axum `Router` and gives you `get` / `post` /
`put` / `patch` / `delete` plus a fluent assertion API on the `TestResponse`. The
`open_then_get` test above, rewritten with `TestClient`:

```rust,ignore
use firefly_testkit::TestClient;

#[tokio::test]
async fn open_then_get_with_testclient() {
    let client = TestClient::new(build_router().await);

    let created = client
        .post("/api/v1/wallets", &serde_json::json!({ "owner": "alice", "openingBalance": 1000 }))
        .await
        .assert_status(201);
    let id = created.json_path("$.id").unwrap();

    client
        .get(&format!("/api/v1/wallets/{}", id.as_str().unwrap()))
        .await
        .assert_status(200)
        .assert_json_path("$.balance", 1000);
}
```

`assert_status`, `assert_success`, `assert_header`, `assert_body_contains`,
`assert_json_eq`, `assert_json_path` / `assert_json_path_exists` /
`assert_json_path_absent`, and `json::<T>()` / `json_path("$.field")` cover the
common assertions; the path grammar is the single-result JSONPath subset (a
leading `$`, dotted or bracketed member access, array indexing). Each assertion
returns `&Self` so they chain. (Blocking variants — `post_blocking`,
`get_blocking`, … — exist for non-async test contexts.)

### Slice — a focused DI container for a test

`Slice` builds a minimal `firefly-container` for a slice test: register only the
collaborators the unit under test needs, then resolve them. It is the analog of a
Spring `@…Test` slice — the container without the full application context:

```rust,ignore
use firefly_testkit::Slice;
use firefly_container::{Container, ContainerError, Scope};

let slice = Slice::new()
    .instance(ReadModel::default())                       // a ready instance (a "mock_bean")
    .register::<MyService, _>(Scope::Singleton, |c: &Container| {
        Ok(MyService::new())                              // a factory; resolve deps from `c`
    })
    .build();

let read_model: std::sync::Arc<ReadModel> = slice.get();
```

`register` / `register_named` take a factory `|c: &Container| -> Result<T,
ContainerError>` (it may resolve its own dependencies from `c`); `instance` /
`instance_named` install a ready value (the override/mock path); `bind` coerces a
concrete type to a trait object; and `eager` forces construction at build time.
`build()` returns a `BuiltSlice` you resolve from with `get::<T>()` /
`get_named::<T>(name)`.

### Asserting emitted events

`SpyBroker` records what a handler published; `assert_event_published(&spy,
"Type")` asserts an event of that type was recorded and returns it (the
`_with` variant also checks the payload contains a substring;
`assert_no_events_published` asserts none). `must_encode` / `must_decode` are
panic-on-failure JSON helpers. A Lumen-flavored example — proving an open emits a
`WalletOpened`:

```rust,ignore
use firefly_testkit::{assert_event_published, must_encode, SpyBroker};

#[test]
fn open_emits_wallet_opened() {
    let spy = SpyBroker::new();
    // The ledger would publish through the broker; here we record the envelope
    // the projection would consume.
    spy.record("wallets.events", "WalletOpened",
               &must_encode(&serde_json::json!({ "id": "wlt_1", "owner": "alice" })));

    let event = assert_event_published(&spy, "WalletOpened");
    assert_eq!(event.topic, "wallets.events");
}
```

### Webhook signers

When Lumen grows an inbound webhook (Chapter 16), the testkit's HMAC signers —
`sign_hmac`, `sign_stripe`, `sign_github`, `sign_twilio` — produce header values
byte-identical to what each `firefly-webhooks` validator expects, so a signed
test request validates exactly as a real provider's would:

```rust,ignore
use firefly_testkit::sign_stripe;

let sig = sign_stripe(b"whsec_test", br#"{"type":"charge.succeeded"}"#, 1_700_000_000);
// Attach `sig` as the `Stripe-Signature` header on a TestClient POST.
```

## Testing reactive pipelines

The streaming endpoint (Chapter 20) builds a `Flux`. A reactive pipeline is
tested by driving it to a terminal — `block()`, `collect_list()`, `count()` — and
asserting the resolved value. This is the `firefly-reactive` analog of Reactor's
`StepVerifier`:

```rust
use firefly_reactive::Flux;

#[tokio::test]
async fn pipeline_filters_and_maps() {
    let out = Flux::range(1, 5)
        .filter(|x| x % 2 == 1)
        .map(|x| x * 10)
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out, vec![10, 30, 50]);
}
```

Lumen's streaming tests (`tests/streaming.rs`, gated behind the `streaming`
feature) take the HTTP route instead: open a wallet, deposit, then `GET
/events` and assert two NDJSON lines (`WalletOpened` + `MoneyDeposited`) by
default, `text/event-stream` with `?format=sse`, and a 404 for an unknown wallet.

## Real-infrastructure integration tests

Lumen runs hermetically, but the production adapters need real services. The
workspace ships a `docker-compose.yml` with Postgres, Redis, RabbitMQ, a
Kafka-compatible Redpanda, Keycloak, S3/Blob emulators, and an SMTP capture. The
convention throughout the adapter crates is: a test reads a connection URL from
the environment and **skips when it is unset**, so the default `cargo test` stays
green on a bare machine while CI flips the full suite on:

```rust,ignore
#[tokio::test]
#[ignore = "requires postgres (DATABASE_URL)"]
async fn postgres_event_store_round_trips() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return }; // skip on a bare machine
    // ... drive the Postgres-backed EventStore against the live database
}
```

```bash
docker compose up -d                       # start the backing services
DATABASE_URL=postgres://firefly:firefly@localhost:5432/firefly \
REDIS_URL=redis://localhost:6379/0 \
  cargo test --workspace -- --ignored      # run the env-gated suite
docker compose down
```

## Running Lumen's suite

From the workspace root (with `export PATH="/opt/homebrew/bin:$PATH"`):

```bash
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                      # 34 unit + 7 HTTP + 1 doctest
cargo test   -p firefly-sample-lumen --features streaming # + 3 streaming tests
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
cargo fmt    -p firefly-sample-lumen -- --check
```

If a snippet in any chapter drifts from the file, this gate fails — which is how
the book stays honest.

## What changed in Lumen

Nothing in `src/` — this chapter is the retrospective on the test code that grew
alongside every feature:

- **Unit tests** per module: `money` and `domain` invariants, `commands`
  validation + bus dispatch, `security` mint/verify/reject, `transfer` happy +
  compensation, `housekeeping` registration + tick.
- The **`tests/http.rs`** end-to-end suite drives the real `build_router()` with
  `tower::oneshot`, covering open → get → deposit/withdraw → transfer (happy +
  compensated) → projection convergence → 401/422/404 problems.
- **`tests/streaming.rs`** (feature-gated) exercises the NDJSON / SSE endpoint.
- The **`firefly-testkit`** helpers — `TestClient`, `Slice`,
  `assert_event_published`, the HMAC signers — are the terse path to the same
  coverage in your own service.

## Exercises

1. **Rewrite a test with `TestClient`.** Take
   `deposit_and_withdraw_update_the_balance` from `tests/http.rs` and rewrite it
   using `TestClient` + `assert_json_path`. Add the `Authorization` header via
   `TestClient::request` so the mutation is authenticated.
2. **A `Slice` test for the read model.** Use `Slice` to register a
   `ReadModel::default()` instance, project a `WalletOpened` into it by hand, and
   assert `find` returns the view — all without the bus or the router.
3. **Event assertion on the ledger.** Wire a `SpyBroker` into a `Ledger` in a
   test, commit a deposit, and use `assert_event_published_with(&spy,
   "MoneyDeposited", "50")` to prove the amount is on the wire.
4. **A skipping integration test.** Write an `#[ignore]`d test that reads
   `DATABASE_URL`, returns early when unset, and otherwise opens a wallet against
   a Postgres-backed event store. Confirm it skips with a plain `cargo test` and
   runs with the variable set.

With the stack proven at every level, the remaining chapters cover the CLI and
shipping Lumen to production. Continue to [The CLI](./19-cli.md).
