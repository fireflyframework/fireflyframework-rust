# Testing

Every chapter so far has shown Lumen's listings *and* the tests that keep them
honest — that is the whole point of the book: the prose is verified against a
crate that compiles and passes its suite. This chapter steps back from any one
feature and looks at the test strategy as a whole — the way you would design it
for your own service. You will not learn a single new business rule here; you
will learn how to prove the ones you already wrote, at three levels, without
booting a server or starting a database.

The good news is that Firefly's in-memory-first stack makes almost every test a
plain function call. Lumen's default infrastructure — its event store, event
broker, and read model — is pure Rust running in-process, so a test never binds
a socket, never opens a connection, and never waits on a container. The result
is a suite that is fast, deterministic, and green on a bare laptop.

By the end of this chapter you will:

- Understand Firefly's three testing tiers — pure unit tests, in-process HTTP
  tests that drive the *real* router, and env-gated integration tests against
  live infrastructure — and when to reach for each.
- Drive a fully-wired application router in-process with `bootstrap()` and
  `tower::oneshot`, with no socket bound and no mocks.
- Use the `firefly-testkit` helpers — `TestClient`, `Slice`,
  `assert_event_published`, and the webhook signers — to write the same tests
  far more tersely.
- Build a focused dependency-injection slice for a single unit, install a fake
  collaborator (the `@MockBean` analog), and drive one controller over mocks
  (the `@WebMvcTest` analog).
- Write an integration test that uses real Postgres or Kafka when present and
  **skips cleanly** when it is not, so `cargo test` stays green everywhere.

## Concepts you will meet

Before the first test, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — testing tier.** A *tier* is one layer of the test
> pyramid: pure unit tests at the bottom (fastest, most numerous), in-process
> HTTP/slice tests in the middle, and integration tests against live
> infrastructure at the top (slowest, fewest). Firefly gives you one terse helper
> per tier. The split mirrors the JUnit + Spring Boot test stack: plain `@Test`,
> `@SpringBootTest` / `@WebMvcTest`, and `@Testcontainers`.

> **Note** **Key term — in-process HTTP test.** An *in-process* test drives the
> real HTTP router by handing it a `Request` and `await`ing the `Response`
> directly — no port is opened and no server task is spawned. It is the speed of
> a unit test with the coverage of an end-to-end test. The Spring analog is
> `MockMvc` (and Spring's `WebTestClient` in `MOCK` mode).

> **Note** **Key term — test seam.** A *seam* is a place the framework exposes
> specifically so tests can reach inside. Firefly's seam is `bootstrap()`: it
> assembles the same fully-wired application `run()` would serve, but hands it
> back as a value *without* binding a socket. Spring's `@SpringBootTest` boots
> the same context the production `main` does; `bootstrap()` is its Rust analog.

> **Note** **Key term — mock / fake.** A *fake* is a stand-in collaborator you
> install in place of the real one — an in-memory repository instead of a
> database, a canned service instead of a network call. Installing one is the
> `@MockBean` move from Spring: override a bean under its port so the unit under
> test wires the fake instead of the real implementation.

## The in-process testing model

Lumen's default stack is entirely in-memory — a `MemoryEventStore`, an
`InMemoryBroker`, and a `Mutex<HashMap>` read model — so almost every test runs
as a plain `#[tokio::test]` with **no socket and no external service**. Even the
HTTP tests do not bind a port: they hand a `Request` to the router and `await`
the `Response`. That single fact is what makes the suite fast and CI-friendly,
and it is worth stating up front because every tier below is built around it.

The model has one organizing rule: each test boots **one** application context
and drives every request against it. That is exactly Spring Boot's
`@SpringBootTest` model — one wired context per test method — and in Lumen the
helper that gives it to you is `build_router()`:

```rust,ignore
// src/web.rs — the test seam, compiled only under #[cfg(test)].
#[cfg(test)]
pub(crate) async fn build_router() -> axum::Router {
    firefly::FireflyApplication::new(APP_NAME)
        .version(VERSION)
        .bootstrap()
        .await
        .expect("lumen bootstrap")
        .api_router
}
```

What just happened: `bootstrap()` runs the same boot pipeline as `run()` —
component-scan the DI container, auto-mount every `#[rest_controller]`,
auto-discover security and middleware, drain the inventory-registered CQRS
handlers / EDA listeners / `#[scheduled]` tasks — and returns a `Bootstrapped`
value instead of serving it. Its `.api_router` field is the public
`axum::Router`, fully wired, with no listener bound. `build_router()` is just
`main()` minus the `.run()` serve step.

> **Note** **Key term — bootstrap seam.** `bootstrap()` is the sibling of
> `run()` you met in [Quickstart](./02-quickstart.md): `run()` assembles the app
> *and serves it*; `bootstrap()` assembles the identical app and returns the
> `Bootstrapped` handle so a test can drive `Bootstrapped::api_router`
> in-process. Same beans, same wiring, no socket.

Because the CQRS handlers (`WalletHandlers`) and the read-model projection
(`WalletProjection`) are **autowired DI beans** — not free functions over a
process-global — each test's container is self-consistent. The `Ledger`,
`ReadModel`, and `QueryCache` singletons that one container resolves are the
*same* instances every handler and the projection share. So a wallet a command
opens is the wallet a later query reads, because both run against the one
container the test booted. And since an `axum::Router` is cheap to `clone` (it is
`Arc`-backed), each request clones the shared app rather than rebuilding it.

> **Tip** **Checkpoint.** You can already run the whole suite. From the workspace
> root, `cargo test -p firefly-sample-lumen` builds Lumen and runs its tests; you
> should see `42 unit + 12 HTTP + 1 doctest` pass. The rest of this chapter
> explains what those tests *are*.

## Tier 1 — Unit tests with no infrastructure

The bottom tier needs nothing: no router, no container, no I/O. Lumen's value
object and aggregate are pure Rust, so their tests construct a value and assert
an invariant directly. `money.rs` and `domain.rs` check exact-cents arithmetic,
positive amounts, sufficient funds, and the "owner required" rule with plain
`assert!`s.

The CQRS layer is just as direct. The handlers live on a `#[derive(Service)]`
bean (`WalletHandlers`) whose collaborators — the write-side `Ledger` and the
read-side `ReadModel` — are `#[autowired]` from the container at boot. But
nothing stops you from constructing the bean yourself with those collaborators in
hand and calling a method straight. This is the heart of `commands.rs`'s test
module:

```rust,ignore
use firefly::eda::InMemoryBroker;
use firefly::eventsourcing::MemoryEventStore;

#[tokio::test]
async fn handler_bean_operates_on_its_autowired_collaborators() {
    // Build the handler bean with the same Ledger + ReadModel the container
    // would inject — no bus, no process-global, no boot.
    let handlers = WalletHandlers {
        ledger: Arc::new(Ledger::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )),
        read_model: Arc::new(ReadModel::default()),
    };

    let opened = handlers
        .open_wallet(OpenWallet { owner: "alice".into(), opening_balance: 100 })
        .await
        .unwrap();
    assert_eq!(opened.balance, 100);

    let after = handlers
        .deposit(Deposit { wallet_id: opened.id.clone(), amount: 50 })
        .await
        .unwrap();
    assert_eq!(after.balance, 150);
}
```

What just happened, and why it matters: you built the handler bean by hand with
an in-memory `Ledger` and a fresh `ReadModel`, then called `open_wallet` and
`deposit` directly and asserted the returned balances. No bus dispatch, no DI
container, no HTTP. The full application boot installs the *same* bean on the bus
by draining the inventory registry (`register_discovered_handlers`), so this test
exercises the real handler logic without standing any of that up. When you want
to know "does the handler do the right arithmetic?", this is the cheapest place
to find out.

Validation is tested the same way — without ever touching HTTP. `OpenWallet`
carries `#[derive(Command)]`, which generated a `.validate()` from its
`#[firefly(validate)]` fields, so you call it on the command directly:

```rust,ignore
#[test]
fn open_wallet_validates_owner() {
    assert!(OpenWallet::default().validate().is_err());      // empty owner fails
    assert!(OpenWallet { owner: "alice".into(), opening_balance: 0 }.validate().is_ok());
}
```

What just happened: the empty default fails validation (no owner), and a
well-formed command passes — all before any handler runs. The web layer never
sees an invalid command because the bus rejects it first; this test pins that
rejection at the cheapest possible level.

> **Note** Security (`security.rs`), the transfer saga (`transfer.rs`), the
> compliance workflow (`compliance.rs`), the two-phase transfer
> (`tcc_transfer.rs`), and the scheduled task (`housekeeping.rs`) each carry
> their own `#[cfg(test)] mod tests` in the same spirit: mint-then-verify a
> token, run the saga happy path *and* its compensation path, run the workflow's
> approve/reject branches, and register the heartbeat and assert it ticks. These
> are the chapters [Security](./14-security.md), [Sagas, Workflows &
> TCC](./12-sagas.md), and [Scheduling &
> Notifications](./16-scheduling-notifications.md) proving themselves.

> **Tip** **Checkpoint.** Together these account for Lumen's **42 unit tests**:
> `money` and `domain` invariants, `commands` validation plus the handler bean,
> `security` mint/verify/reject, `transfer`/`tcc_transfer` happy + compensation,
> `compliance` approve/reject, and `housekeeping` registration + tick. Run
> `cargo test -p firefly-sample-lumen --lib` to see just these.

## Tier 2 — In-process HTTP tests with `tower::oneshot`

The middle tier proves the whole stack composes. Lumen's end-to-end suite lives
in `src/http_test.rs` — a `#[cfg(test)] mod http_test` declared in `main.rs`, so
it runs as part of the binary's own test target — and drives the **fully-wired**
`build_router()`: the auto-mounted `#[rest_controller]` routes, the CQRS handler
bean, the event-sourced ledger, the read-model projection bean, the transfer
saga, *and* the auto-discovered JWT/RBAC enforcement from
[Security](./14-security.md). No mocks: every layer is the production layer, just
over in-memory infrastructure.

> **Note** **Key term — `tower::oneshot`.** `oneshot` (from
> `tower::ServiceExt`) sends exactly one request through a `Service` — here an
> `axum::Router` — and resolves to its `Response`, then drops the service. It is
> how you call a router as a plain async function. The router's body type comes
> from `http_body_util::BodyExt`, which you use to collect the response bytes.

### Step 1 — Write the request/response helpers

The pattern is one `Router` per test plus `oneshot` per request. A test boots the
app once with `let app = build_router().await` and drives every request against
it; a small `send` helper clones the shared `&Router` per request so they all
share the one container. Here are the helpers `http_test.rs` defines once at the
top of the file:

```rust,ignore
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Sends one request against the (cloned) shared app and returns the response.
async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.unwrap()
}

/// Builds a POST with a JSON body, optionally carrying a bearer token.
fn post(path: &str, body: serde_json::Value, auth: bool) -> Request<Body> {
    let mut b = Request::post(path).header("content-type", "application/json");
    if auth {
        b = b.header("authorization", bearer()); // "Bearer <minted CUSTOMER token>"
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()
}

/// Buffers the response body and decodes it as JSON into `T`.
async fn body_json<T: serde::de::DeserializeOwned>(res: Response) -> T {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
```

What just happened: `send` is the whole mechanism — `app.clone().oneshot(req)`
runs the request through the real router in-process. `post` assembles a JSON
request and, when `auth` is true, attaches an `Authorization: Bearer …` header
minted by `bearer()` (which calls Lumen's `mint_token("u-alice",
&[CUSTOMER_ROLE])` from the security module). `body_json` drains the response body
with `BodyExt::collect` and deserializes it. Three helpers, and every test below
reads like a script.

### Step 2 — Drive a round-trip through CQRS

With the helpers in place, a test boots the app, opens a wallet through the
public API, and asserts the projected read comes back through CQRS — all against
the one app context:

```rust,ignore
#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    let app = build_router().await;                       // one app context per test
    let opened = open_wallet(&app, "alice", 1_000).await; // POST /api/v1/wallets, asserts 201
    assert_eq!(opened.owner, "alice");
    assert_eq!(opened.balance, 1_000);

    // GET dispatches the #[query_handler] on the handler bean; it reads the
    // projection (or repairs from the event stream) — both resolved from the
    // SAME container as the command that opened the wallet.
    let fetched = get_wallet(&app, &opened.id).await;
    assert_eq!(fetched.id, opened.id);
    assert_eq!(fetched.balance, 1_000);
}
```

What just happened, and why it matters: the `POST` ran a command through the bus,
which appended events to the in-memory ledger; the `GET` ran a query that read
the projection those events fed. Both resolved the *same* `Ledger` and
`ReadModel` from the one container the test booted, so the read sees the write.
This single test proves the command side, the query side, the projection, and
their shared wiring all fit together — something no unit test can show, because
the seam being tested *is* the wiring.

### Step 3 — Prove the failure modes render as problems

The same file proves the saga happy path
(`transfer_saga_happy_path_moves_funds_between_wallets`), the compensation path
(`transfer_saga_overdraft_compensates_and_is_422`), and the problem-rendering for
the failure modes. A missing token is a 401, an empty owner is a 422, and an
unknown id is a 404 — each asserting the `application/problem+json` content type:

```rust,ignore
#[tokio::test]
async fn missing_token_is_401_problem_on_mutations() {
    let app = build_router().await;
    let res = send(
        &app,
        post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "mallory", "openingBalance": 10 }),
            false, // no Authorization header
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(content_type(&res).contains("application/problem+json"));
}
```

What just happened: the unauthenticated `POST` was rejected by the
auto-discovered security layer with a 401, *and* the body came back as an RFC
9457 `application/problem+json` document — not a blank 401. The same shape holds
for the 422 (validation) and 404 (unknown wallet) tests. That single suite is the
proof that the whole stack — routing, security, CQRS, event sourcing, sagas, and
problem rendering — composes correctly.

> **Note** **Key term — RFC 9457 problem response.** RFC 9457 (which obsoletes
> the older RFC 7807) defines `application/problem+json`: a structured error body
> with a `type`, `title`, `status`, and `detail`. Firefly renders every handler
> error and every unmatched route as one automatically, which is why the tests
> can assert on the content type. You met this in [Your First HTTP
> API](./06-first-http-api.md).

> **Tip** **Checkpoint.** These twelve scenarios — open → get → deposit/withdraw
> → transfer (happy + compensated) → compliance workflow → two-phase transfer →
> 401/422/404 problems — are Lumen's **12 HTTP tests**. Run `cargo test
> -p firefly-sample-lumen --test '*' 2>/dev/null || cargo test
> -p firefly-sample-lumen` and watch the `http_test` module pass.

## Tier 2, the terse way — the `firefly-testkit`

Lumen's own HTTP tests use the raw `tower::oneshot` form on purpose, to show the
mechanism with no magic. In *your* service you would reach for `firefly-testkit`,
which packages exactly that boilerplate into reusable helpers. It is a separate
crate with feature-gated tiers, so you only pull in what you use:

```toml
# Cargo.toml — add as a dev-dependency, switching on the helpers you need.
[dev-dependencies]
firefly-testkit = { version = "26.6.28", features = ["web", "container"] }
```

> **Note** The default surface (the webhook signers, `SpyBroker`, and the JSON
> helpers) carries no heavy dependencies. The `web` feature adds the in-process
> `TestClient`; `container` adds the DI `Slice`; and `testcontainers` adds the
> integration-test fixtures. A service that only signs webhooks gets a lean
> build.

Three pieces matter most.

### TestClient — an in-process HTTP client (feature `web`)

`TestClient::new(router)` wraps any axum `Router` and gives you `get` / `post` /
`put` / `patch` / `delete` (async) plus a fluent assertion API on the
`TestResponse` it returns. The `open_then_get` test above, rewritten with
`TestClient`:

```rust,ignore
use firefly_testkit::TestClient;

#[tokio::test]
async fn open_then_get_with_testclient() {
    let client = TestClient::new(build_router().await);

    let created = client
        .post("/api/v1/wallets", &serde_json::json!({ "owner": "alice", "openingBalance": 1000 }))
        .await;
    created.assert_status(201);
    let id = created.json_path("$.id").unwrap();

    client
        .get(&format!("/api/v1/wallets/{}", id.as_str().unwrap()))
        .await
        .assert_status(200)
        .assert_json_path("$.balance", 1000);
}
```

What just happened: `TestClient` did the request-building and body-buffering for
you. `post(path, &body)` serializes the JSON and sets `content-type`;
`assert_status` checks the code; `json_path("$.id")` selects a single field; and
`assert_json_path("$.balance", 1000)` asserts one value deep in the body without
spelling out the whole document. Each assertion returns `&Self`, so they chain.

The assertion surface is: `assert_status`, `assert_success`, `assert_header` /
`assert_header_present`, `assert_body_contains`, `assert_json_eq`,
`assert_json_path` / `assert_json_path_exists` / `assert_json_path_absent`, plus
the extractors `json::<T>()`, `json_path("$.field")`, `text()`, `header(name)`,
and `body_bytes()`. The path grammar is a single-result JSONPath subset: a
leading `$`, dotted (`$.user.name`) or bracketed (`$['user']['name']`) member
access, and array indexing (`$[0]`, `$.items[2].id`) — no wildcards, filters, or
recursive descent.

> **Note** Every verb also has a blocking variant — `get_blocking`,
> `post_blocking`, … — that drives the request on an internal current-thread
> runtime, so a plain `#[test]` (no `#[tokio::test]`) reads exactly like a
> synchronous HTTP client. Use the blocking form outside a Tokio runtime and the
> async form inside one.

### Slice — a focused DI container for one test (feature `container`)

The HTTP tests boot the *whole* application. Sometimes you want the opposite: the
wiring for a single unit and nothing else — no router, no datasource. `Slice`
builds a minimal `firefly-container` for exactly that. You register only the
collaborators the unit under test needs, then resolve them.

> **Note** **Key term — slice test.** A *slice* loads a focused subset of the
> object graph instead of the whole application context. It is faster than a full
> boot and isolates the unit under test. Spring's slice annotations
> (`@WebMvcTest`, `@DataJpaTest`) are the direct analog; `Slice` is the explicit
> builder Rust needs in their place, since there is no package scanning.

```rust,ignore
use firefly_testkit::Slice;
use firefly_container::{Container, ContainerError, Scope};

let slice = Slice::new()
    .instance(ReadModel::default())                       // a ready instance (the mock/override path)
    .register::<MyService, _>(Scope::Singleton, |c: &Container| {
        Ok(MyService::new())                              // a factory; resolve deps from `c`
    })
    .build();

let read_model: std::sync::Arc<ReadModel> = slice.get();
```

What just happened: `instance(value)` installs a ready singleton; `register::<T,
_>(scope, factory)` registers a bean built by a factory that can resolve its own
dependencies from the container `c`; and `build()` returns a `BuiltSlice` you
resolve from with `get::<T>()` (or `get_named::<T>(name)`). There is also
`eager::<T>()`, which forces a bean's construction at `build()` time so a missing
collaborator fails *there* (the fail-fast gate that mirrors Spring's slice
startup) rather than lazily on first use.

The `instance` + `bind` pair **is** the `@MockBean`. Install a fake under a port
and the bean under test wires it instead of the real collaborator:

```rust,ignore
let slice = Slice::new()
    .instance(FakeRepo::default())             // the fake (a "mock_bean")
    .bind::<dyn Repo, FakeRepo>(|a| a)         // expose it as the `dyn Repo` port
    .register::<Service, _>(Scope::Singleton, |c| {
        Ok(Service { repo: c.resolve::<dyn Repo>()? })  // wires the fake
    })
    .eager::<Service>()                        // fail fast if `Repo` is missing
    .build();
```

Because the fake is held by the container, `get::<FakeRepo>()` after `build()`
hands back the *same* instance the service wired in. So you configure and assert
against it through interior mutability — the mock-verification move from Spring,
without a mocking framework.

### `@WebMvcTest` — one controller over mocked services with `web_client`

Combine the two: register a controller bean plus its **mocked** collaborators,
then call `built.web_client::<C, _>(C::routes)` to resolve that controller and
wrap its `#[rest_controller]`-generated router in a `TestClient`. This is Spring's
`@WebMvcTest(Controller.class)` + `@MockBean(Service.class)` — one controller's
web layer exercised over fakes, with no full-application boot and no datasource:

```rust,ignore
use firefly_testkit::Slice;
use firefly_container::Scope;

// @WebMvcTest(WalletController) + @MockBean(WalletService)
let client = Slice::new()
    .instance(FakeWalletService::default())               // the mock
    .bind::<dyn WalletService, FakeWalletService>(|a| a)
    .register::<WalletController, _>(Scope::Singleton, |c| {
        Ok(WalletController { service: c.resolve::<dyn WalletService>()? })
    })
    .eager::<WalletController>()
    .build()
    .web_client::<WalletController, _>(WalletController::routes);

client.get_blocking("/api/v1/wallets/unknown").assert_status(404);
```

What just happened: `web_client` (feature `web`) takes the controller's generated
`fn routes(state: C) -> Router`, clones the resolved bean into the router's state,
and wraps the result in a `TestClient`. The whole web layer of one controller is
now driven over fakes. (`FakeWalletService` / `WalletController` here are
illustrative shapes for *your* service — Lumen's own controller autowires the
real bus, so its web coverage comes from the Tier 2 HTTP tests above.)

> **Note** For a **`@DataJpaTest`** — a persistence slice with no web stack — the
> same `Slice` registers a repository over an in-memory SQLite database. Build the
> repository with `firefly::data_sqlx::repository_for::<Entity>(db)`, exactly as
> `lumen-ledger`'s `-models` tests do: they point a `Db` at an in-memory SQLite
> URL (`sqlite:file:…?mode=memory&cache=shared`) and exercise the real derived
> queries with no Postgres in sight. You met those repositories in [Persistence &
> Reactive Repositories](./07-persistence.md).

### Asserting emitted events with `SpyBroker`

The third everyday helper proves a handler *published* the right event.
`SpyBroker` records what a handler published, and the assertion helpers read it
back:

- `assert_event_published(&spy, "Type")` asserts an event of that type was
  recorded and returns it.
- `assert_event_published_with(&spy, "Type", &json)` also checks the payload
  (parsed as a JSON object) contains the given key/value pairs — a *subset* match,
  so extra fields are ignored.
- `assert_no_events_published(&spy)` asserts none were recorded.
- `must_encode` / `must_decode` are panic-on-failure JSON helpers for building
  and reading payloads.

A Lumen-flavored example — proving an open emits a `WalletOpened`:

```rust,ignore
use firefly_testkit::{assert_event_published, must_encode, SpyBroker};

#[test]
fn open_emits_wallet_opened() {
    let spy = SpyBroker::new();
    // The ledger publishes through the broker; here we record the envelope the
    // projection would consume.
    spy.record(
        "wallets.events",
        "WalletOpened",
        &must_encode(&serde_json::json!({ "id": "wlt_1", "owner": "alice" })),
    );

    let event = assert_event_published(&spy, "WalletOpened");
    assert_eq!(event.topic, "wallets.events");
}
```

What just happened: `spy.record(topic, type, payload)` stores an event envelope,
and `assert_event_published` finds the first one of the named type (or fails the
test, listing what *was* published). The returned `RecordedEvent` carries
`topic`, `event_type`, and the raw `payload` bytes, so you can assert further.
Wire a `SpyBroker` into a `Ledger` in a real test and you can prove a deposit
emits a `MoneyDeposited` with the right amount.

### Webhook signers

When Lumen grows an inbound webhook (the [Scheduling &
Notifications](./16-scheduling-notifications.md) chapter), the testkit's HMAC
signers — `sign_hmac`, `sign_stripe`, `sign_github`, `sign_twilio` — produce
header values byte-identical to what each `firefly-webhooks` validator expects, so
a signed test request validates exactly as a real provider's would:

```rust,ignore
use firefly_testkit::sign_stripe;

let sig = sign_stripe(b"whsec_test", br#"{"type":"charge.succeeded"}"#, 1_700_000_000);
// Attach `sig` as the `Stripe-Signature` header on a TestClient POST and the
// validator accepts it exactly as it would a real Stripe delivery.
```

What just happened: `sign_stripe(secret, body, unix_ts)` builds the
`t=<unix>,v1=<hex>` value Stripe sends in `Stripe-Signature`, signing
`<unix>.<body>` with HMAC-SHA256. Because the signer matches the validator's wire
shape exactly, a test that signs its own payload proves your receiver accepts a
genuine delivery.

## Testing reactive pipelines

The streaming endpoint (introduced in [Production &
Deployment](./20-production.md)) builds a `Flux`. You met `Mono` and `Flux` in
[The Reactive Model](./05-reactive-model.md); here is how you *test* one.

> **Note** **Key term — terminal operation.** A reactive pipeline is lazy: the
> operators (`filter`, `map`, …) describe work but run nothing until a *terminal*
> consumes the stream. `collect_list()`, `count()`, and `block()` are terminals —
> they drive the pipeline to completion and resolve a value. Spring Reactor's
> `block()` / `collectList()` are the direct analog.

You test a pipeline by driving it to a terminal and asserting the resolved value:

```rust
use firefly_reactive::Flux;

#[tokio::test]
async fn pipeline_filters_and_maps() {
    let out = Flux::range(1, 5)          // emits 1, 2, 3, 4, 5 (start, count)
        .filter(|x| x % 2 == 1)          // keep the odds: 1, 3, 5
        .map(|x| x * 10)                 // scale: 10, 30, 50
        .collect_list()                  // Flux<i64> -> Mono<Vec<i64>>
        .block()                         // Result<Option<Vec<i64>>, FireflyError>
        .await
        .unwrap()                        // unwrap the Result
        .unwrap();                       // unwrap the Option (the stream was non-empty)
    assert_eq!(out, vec![10, 30, 50]);
}
```

What just happened, and why the double `unwrap`: `Flux::range(1, 5)` emits five
values starting at `1`. `filter` and `map` transform them lazily. `collect_list()`
turns the `Flux<i64>` into a `Mono<Vec<i64>>` — a single value holding the whole
list — and `block().await` drives it to completion. `block()` returns
`Result<Option<Vec<i64>>, FireflyError>`: the `Result` surfaces a pipeline error,
and the `Option` is `None` only for an empty stream, so a successful non-empty run
needs both `unwrap`s. This is plain async Rust assertions over a resolved stream —
no special test runtime.

> **Note** Lumen's streaming tests (`src/streaming_test.rs`, gated behind the
> `streaming` feature) take the HTTP route instead of testing the `Flux` directly:
> they open a wallet, deposit, then `GET /events` and assert two NDJSON lines
> (`WalletOpened` + `MoneyDeposited`) by default, `text/event-stream` with
> `?format=sse`, and a 404 for an unknown wallet. Those are the `+3 streaming
> tests` you turn on with `--features streaming`.

## Tier 3 — Real-infrastructure integration tests

Lumen runs hermetically, but the production adapters you reach for in [Production
& Deployment](./20-production.md) need real services. The workspace ships a
`docker-compose.yml` with Postgres, Redis, RabbitMQ, a Kafka-compatible Redpanda,
Keycloak, S3/Blob emulators, and an SMTP capture.

The convention throughout the adapter crates keeps the default `cargo test` green
on a bare machine: a test reads a connection URL from the environment and
**skips when it is unset**. CI flips the full suite on by exporting the variable.

> **Note** **Key term — env-gated test.** An *env-gated* test only runs when a
> named environment variable is present (a `DATABASE_URL`, a `REDIS_URL`).
> Marking it `#[ignore]` keeps it out of the default run; reading the variable and
> returning early means even `--ignored` skips cleanly where the service is
> absent. This is the Rust analog of Spring's `@Testcontainers` /
> `@EnabledIf`-guarded tests.

```rust,ignore
#[tokio::test]
#[ignore = "requires postgres (DATABASE_URL)"]
async fn postgres_event_store_round_trips() {
    // Skip on a bare machine: no DATABASE_URL -> return before touching the DB.
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    // ... drive the Postgres-backed EventStore against the live database at `url`.
}
```

What just happened: the `#[ignore]` keeps this test out of `cargo test`'s default
run entirely. When you opt in with `--ignored`, the `let … else { return }` guard
still skips cleanly if `DATABASE_URL` is unset, so the only way it actually
touches Postgres is when you point it at a live one. To run the env-gated suite,
start the backing services and export the URLs:

```bash
docker compose up -d                       # start the backing services
DATABASE_URL=postgres://firefly:firefly@localhost:5442/firefly \
REDIS_URL=redis://localhost:6379/0 \
  cargo test --workspace -- --ignored      # run the env-gated suite
docker compose down
```

> **Note** The compose file maps Postgres to host port **5442** (not the default
> 5432) to avoid colliding with a local Postgres you may already run — which is
> why the `DATABASE_URL` above says `localhost:5442`.

The testkit can shorten this tier too. With the `testcontainers` feature,
`firefly_testkit::containers` maps a started service's `(host, port)` to the
canonical `firefly.*` config keys (`config_for(&container)`) and offers a
`docker_available()` skip guard — the Rust analog of Spring's
`@ServiceConnection`. It is decoupled from any specific container library: feed it
the connection details any tool already hands you.

## Running Lumen's suite

From the workspace root (with `export PATH="/opt/homebrew/bin:$PATH"` on macOS so
the toolchain resolves):

```bash
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                      # 42 unit + 12 HTTP + 1 doctest
cargo test   -p firefly-sample-lumen --features streaming # + 3 streaming tests
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
cargo fmt    -p firefly-sample-lumen -- --check
```

> **Tip** **Checkpoint.** A clean run prints `test result: ok` for the unit and
> HTTP tiers and the doctest, with zero clippy warnings and a clean `fmt --check`.
> If a snippet in any chapter drifts from the file, this gate fails — which is
> precisely how the book stays honest.

## Recap — how Lumen proves itself

Nothing changed in `src/` this chapter; it is the retrospective on the test code
that grew alongside every feature. You now know:

- **The three tiers, and one helper per tier.** Pure `#[tokio::test]` unit tests
  with no I/O; in-process HTTP/slice tests that drive the real router without
  binding a socket; and env-gated integration tests against live infrastructure.
- **`bootstrap()` is the test seam.** It assembles the same fully-wired app
  `run()` would serve and returns `Bootstrapped::api_router` — no socket — so
  `build_router()` gives each test one self-consistent container where a write is
  visible to a later read.
- **Tier 1 — unit tests.** Construct a value object, aggregate, or handler bean
  with its collaborators in hand and assert directly; call `.validate()` on a
  command without HTTP. Lumen's **42 unit tests** live here.
- **Tier 2 — in-process HTTP.** `tower::oneshot` drives `build_router()` end to
  end over in-memory infrastructure; Lumen's **12 HTTP tests** cover open → get →
  deposit/withdraw → transfer (happy + compensated) → workflow → 2PC →
  401/422/404 RFC 9457 problems. `firefly-testkit`'s `TestClient`, `Slice`
  (`@MockBean` / `@WebMvcTest` / `@DataJpaTest`), and `SpyBroker` make the same
  coverage terse in your own service.
- **Reactive pipelines** are tested by driving a `Flux` to a terminal
  (`collect_list().block()`) — the chapter's single **doctest**.
- **Tier 3 — integration.** `#[ignore]`d, env-gated tests read a connection URL,
  skip cleanly when it is unset, and run against `docker compose` services (or the
  testkit's `containers` fixtures) when it is set.

## Exercises

1. **Rewrite a test with `TestClient`.** Take the read assertions from
   `deposit_and_withdraw_update_the_balance` in `src/http_test.rs` and rewrite the
   final `GET` round-trip using `TestClient` + `assert_json_path`. (The
   `TestClient` request helpers carry no per-request header argument, so boot the
   app once, keep the authenticated mutations on the raw `tower::oneshot` form that
   mints a bearer token against that `Router`, then wrap the *same* `Router` in a
   `TestClient` for the public read — one app context, so the read sees the
   mutation.)
2. **A `Slice` test for the read model.** Use `Slice` to register a
   `ReadModel::default()` instance, project a `WalletOpened` into it by hand, and
   assert `find` returns the view — all without the bus or the router. Add
   `.eager::<ReadModel>()` and confirm `build()` succeeds, then resolve it with
   `slice.get::<ReadModel>()`.
3. **Event assertion on the ledger.** Wire a `SpyBroker` into a `Ledger` in a
   test, commit a deposit, and use `assert_event_published_with(&spy,
   "MoneyDeposited", &serde_json::json!({ "amount": 50 }))` to prove the payload's
   `amount` field equals 50. Then add `assert_no_events_published` to a no-op path
   and watch it pass.
4. **A `@WebMvcTest`-style slice.** Sketch a fake service behind a port, register
   it with `.instance(...)` + `.bind::<dyn Port, Fake>(|a| a)`, register a
   controller over it, and call `web_client::<C, _>(C::routes)` to drive one route
   over the fake with `get_blocking`. Assert a 404 for an unknown id.
5. **A skipping integration test.** Write an `#[ignore]`d test that reads
   `DATABASE_URL`, returns early when unset, and otherwise opens a wallet against a
   Postgres-backed event store. Confirm it skips with a plain `cargo test`, skips
   with `--ignored` when the variable is unset, and runs with the variable set.

## Where to go next

- Scaffold, inspect, and operate Lumen with the developer tooling in **[The
  CLI](./19-cli.md)** — including the `firefly` commands that run these same
  checks.
- Swap the in-memory defaults for real Postgres and Kafka, then ship Lumen, in
  **[Production & Deployment](./20-production.md)** — where the Tier 3 integration
  tests finally have live infrastructure to run against.
