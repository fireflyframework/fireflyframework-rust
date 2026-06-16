# Production & Deployment

Lumen has grown across the book from a bare scaffold into a secure, observable,
event-sourced CQRS service with a saga, a workflow, a two-phase transfer, and a
scheduled housekeeping task. Everything you added arrived the same way — by
*declaring a bean* the framework discovers — and the entry point never changed.
This final chapter of the build arc closes the loop: we look at exactly how that
**one-line `main`** boots and shuts down reliably, turn on the optional
**reactive streaming endpoint**, and walk the path from "it works on my machine"
to a container running in production — graceful shutdown, the public/management
port split, environment-driven configuration, packaging, and the swap from the
in-memory event store and broker to durable Postgres and Kafka.

Nothing here rewrites Lumen. The streaming endpoint is one more bean; the
Postgres swap is one bean factory edited in place; the rest is operational
posture. That is the payoff of the port-and-adapter design you have been building
all along.

By the end of this chapter you will:

- Trace `run()` end to end — the eight-stage boot pipeline, the two servers, and
  the graceful SIGINT/SIGTERM drain that maps a clean shutdown to `Ok(())`.
- Add the optional reactive streaming endpoint (`GET /api/v1/wallets/:id/events`
  → NDJSON or SSE) as a feature-gated `RouteContributor` bean, and understand why
  the 404 is resolved before the streaming body starts.
- Serve the actuator on a separate, firewalled management port and point your
  orchestrator's liveness/readiness probes at the right sub-paths.
- Turn on production-hardening middleware through `CoreConfig` and read the
  effective filter chain outermost-to-innermost.
- Swap the in-memory event store and broker for Postgres and Kafka by editing one
  `#[bean]` factory, with nothing downstream changing.
- Package Lumen as a container and check it against a deployment checklist.

## Concepts you will meet

Before the first step, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — graceful shutdown.** *Graceful shutdown* means that when
> the process is asked to stop, it stops accepting new requests, lets in-flight
> requests finish (within a budget), and only then exits. The Spring Boot analog
> is the `server.shutdown=graceful` setting plus the embedded server's drain;
> Firefly does this by default with no configuration.

> **Note** **Key term — management surface.** The *management surface* is the set
> of operational HTTP endpoints — health, info, metrics, environment, log-level
> control — that exist for operators and orchestrators, not for end users.
> Firefly serves them on a separate listener from your business API. This mirrors
> Spring Boot Actuator on a dedicated `management.server.port`.

> **Note** **Key term — RouteContributor.** A *`RouteContributor`* is a bean that
> contributes a sub-router (`axum::Router`) to the public API. The framework
> discovers every `RouteContributor` bean and merges its routes into the assembled
> app, so you can add routes without touching `main` or any `#[rest_controller]`.
> The Spring analog is contributing a `RouterFunction<ServerResponse>` bean that
> the context picks up automatically.

> **Note** **Key term — reactive stream / `Flux`.** A *`Flux<T>`* is a reactive
> sequence of zero-or-more `T` values produced over time with backpressure — the
> Rust analog of Project Reactor's `Flux<T>`. Returned from a handler as
> `application/x-ndjson` or `text/event-stream`, it streams element-by-element to
> the client rather than buffering a whole response.

> **Note** **Key term — port and adapter.** A *port* is an abstract capability
> the domain depends on (here `EventStore`, `Broker`); an *adapter* is a concrete
> implementation of that port (in-memory today, Postgres/Kafka in production). The
> domain talks only to the port, so swapping the adapter changes nothing
> downstream. This is the hexagonal-architecture pattern Spring expresses with
> interfaces and `@Bean` factories.

## Step 1 — Read the one-line `main` one more time

Open `src/main.rs`. After every chapter of the build arc, the entry point is
still a single call:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

What just happened: that one `run()` call component-scans Lumen's beans — the
CQRS bus, the event-sourced ledger, the read-model projection, the query cache,
and the security chain, all over in-memory infrastructure — auto-mounts every
`#[rest_controller]`, auto-discovers security and the read-cache bus middleware,
drains the inventory-registered CQRS handlers / EDA listener / `#[scheduled]`
task, self-hosts the admin dashboard, prints the banner and the line-by-line
startup report, and serves both ports with graceful shutdown. Everything you have
seen chapter by chapter is *declared as a bean*; `main` just hands the crate to
the framework.

> **Note** Lumen's binary boots with an in-memory event store and broker, so
> `cargo run --bin lumen` needs nothing external — no database, no message
> broker. The tests drive the same wiring in-process through `build_router()`,
> which calls `FireflyApplication::bootstrap()` rather than serving on a socket.

> **Tip** **Checkpoint.** `cargo run --bin lumen` prints the Firefly banner, the
> two management URLs, and the startup report, then stays running on `:8080`
> (public) and `:8081` (management). `Ctrl-C` exits cleanly. If that works, the
> rest of this chapter is about what is happening underneath and how to take it to
> production.

## Step 2 — Understand the boot pipeline

There is no hand-written lifecycle wiring in Lumen — `run()` does it all. Under
the hood `run()` is exactly two calls:

```rust,ignore
// firefly::FireflyApplication::run (simplified)
pub async fn run(self) -> Result<(), BoxError> {
    self.bootstrap().await?.serve().await
}
```

`bootstrap()` assembles the fully wired application and returns a `Bootstrapped`
value (the router, the DI container, the scheduler, and the two bind addresses)
*without* serving. `serve()` then runs it on the lifecycle `Application`, which
traps SIGINT/SIGTERM, gives each server task its own drain signal, and grants a
drain budget before exiting. The pipeline, in order:

1. **Build the web stack** and tee logging into the admin capture buffer.
2. **Component-scan the container** — auto-register the framework's infrastructure
   beans, then discover Lumen's `#[derive(Configuration)]` / `#[bean]` factories,
   `#[derive(Controller)]` controllers, and `#[autowired]` fields.
3. **Auto-configure the CQRS bus** — correlation propagation always; the read-cache
   middleware because Lumen declares a `QueryCache` bean.
4. **Auto-discover security** — the `FilterChain` + `BearerLayer` beans
   ([Security](./14-security.md)), layered onto the API with no `.security(...)`
   call.
5. **Auto-mount controllers** — every `#[rest_controller]` and every
   `RouteContributor` bean (including the streaming endpoint added in Step 3),
   then apply the middleware chain and originate W3C trace context.
6. **Drain the discovered handlers** — the CQRS command/query handlers, the EDA
   projection listener, and the `#[scheduled]` housekeeping task, from the
   inventory registries.
7. **Self-host the admin dashboard** on the management port and auto-serve the
   OpenAPI docs (Swagger UI, ReDoc, the OpenAPI 3.1 spec) — all on the management
   port, never the public one.
8. **Print the startup report**, then **serve both ports** with graceful drain.

> **Note** **Key term — `bootstrap()` vs `serve()`.** `bootstrap()` is the test
> seam: it returns the wired `Bootstrapped` app — including
> `Bootstrapped::api_router`, the fully assembled public router — without binding
> a socket, so tests drive the real app in-process. `serve()` is the production
> path that actually listens. `run()` is just `bootstrap().await?.serve().await`.

The two servers and the drain are the part that matters for production:

- **Two servers, two drains.** The public API serves on `:8080` and the
  management surface (`/actuator/*` plus the self-hosted `/admin` dashboard plus
  the API docs) on `:8081`. Each runs on its own task with its own `shutdown`
  handle, so a signal drains both listeners independently — `axum::serve(...)
  .with_graceful_shutdown(shutdown.wait())` per server.
- **`run()` blocks until a signal.** It returns when SIGINT/SIGTERM is received
  and the drain completes. A clean shutdown surfaces internally as a *cancelled*
  error, which `serve()` maps to `Ok(())`; any other error propagates out of
  `main` and the process exits non-zero.
- **Env-overridable binds.** `FIREFLY_SERVER_ADDR` and `FIREFLY_MANAGEMENT_ADDR`
  override the defaults (`0.0.0.0:8080` / `0.0.0.0:8081`), so a container reads
  its ports from the environment with no code change.

The "cancelled is clean" mapping is worth seeing exactly, because it is why
`Ctrl-C` is not an error:

```rust,ignore
// Bootstrapped::serve (the tail of it)
match application.run().await {
    Ok(()) => Ok(()),
    // A handle/signal-triggered stop is a clean shutdown, not a failure.
    Err(err) if err.is_cancelled() => Ok(()),
    Err(err) => Err(Box::new(err)),
}
```

What just happened: the lifecycle `Application` runs both server tasks; a
SIGINT/SIGTERM cancels them, which surfaces as a *cancelled* error; `serve()`
catches exactly that case and returns `Ok(())`, so `main` exits zero. Any genuine
failure (a port already bound, a panic in a server task) propagates and the
process exits non-zero — which is what you want an orchestrator to restart on.

> **Tip** **Checkpoint.** Run `cargo run --bin lumen`, then press `Ctrl-C`. The
> process exits with no stack trace and a zero status (`echo $?` prints `0`).
> That is the cancelled-to-`Ok(())` mapping in action.

## Step 3 — Add the reactive streaming endpoint (feature `streaming`)

Lumen's last endpoint streams a wallet's event history. It is feature-gated so
the teaching baseline stays lean — it needs nothing beyond the `firefly` facade
(`firefly::reactive::Flux` plus `firefly::web::{NdJson, Sse}`). `Cargo.toml`
already declares the flag, off by default:

```toml
# Cargo.toml
[features]
# The reactive streaming endpoint is feature-gated so the teaching baseline
# stays lean; this chapter turns it on. It needs nothing beyond the facade.
default = []
streaming = []
```

### 3a — Declare the route as a bean

The endpoint is wired by **declaring a bean**, not by editing an entry point. A
`#[derive(Service)]` that `provides = "dyn firefly::web::RouteContributor"`
contributes the sub-router; `FireflyApplication` resolves it as the
`dyn RouteContributor` port (Step 2, stage 5) and merges its routes — so a
feature-gated endpoint appears in the API purely because its crate compiled it
in. Add this to `src/web.rs`:

```rust,ignore
// src/web.rs
/// (feature `streaming`) A `RouteContributor` bean adding the reactive
/// `GET /api/v1/wallets/:id/events` endpoint. The framework discovers it
/// (resolved as the `dyn RouteContributor` port) and merges its routes — a
/// feature-gated endpoint wired by declaring a bean, not by a composition root.
#[cfg(feature = "streaming")]
#[derive(Service)]
#[firefly(provides = "dyn firefly::web::RouteContributor")]
struct StreamingRoutes {
    #[autowired]
    api: Arc<WalletApi>,
}

#[cfg(feature = "streaming")]
impl firefly::web::RouteContributor for StreamingRoutes {
    fn routes(&self) -> axum::Router {
        streaming_router((*self.api).clone())
    }
}
```

What just happened, block by block:

- `#[derive(Service)]` makes `StreamingRoutes` a DI bean. The
  `#[firefly(provides = "dyn firefly::web::RouteContributor")]` attribute registers
  it under the `RouteContributor` *port*, so the framework finds it when it
  collects route contributors — you never name `StreamingRoutes` anywhere else.
- `#[autowired] api: Arc<WalletApi>` injects the same controller bean the
  `#[rest_controller]` uses, so the stream reads the very wallets the rest of the
  API writes.
- `impl RouteContributor` returns the sub-router that `streaming_router` builds.
  `RouteContributor::routes(&self) -> axum::Router` is the one method the trait
  requires.

> **Note** Everything in this section is behind `#[cfg(feature = "streaming")]`,
> so with the feature off the file compiles to nothing extra and the endpoint
> does not exist. Turning it on is a build flag, not a code change to `main`.

### 3b — Build the sub-router and the handler

The sub-router maps the one route onto the handler over the controller state, and
the handler loads the wallet's persisted events, maps them to the view shape,
wraps them in a `Flux`, and returns NDJSON by default or SSE when `?format=sse`
is passed:

```rust,ignore
// src/web.rs
/// Builds the streaming sub-router over the controller state.
#[cfg(feature = "streaming")]
fn streaming_router(api: WalletApi) -> axum::Router {
    axum::Router::new()
        .route(
            "/api/v1/wallets/:id/events",
            axum::routing::get(stream_events),
        )
        .with_state(api)
}

/// The reactive streaming handler: builds a `Flux<WalletEvent>` over the
/// wallet's persisted stream and returns it as NDJSON (one JSON document per
/// line) or, with `?format=sse`, as Server-Sent Events.
#[cfg(feature = "streaming")]
async fn stream_events(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<StreamParams>,
) -> Response {
    use crate::domain::WalletEvent;
    use axum::response::IntoResponse;
    use firefly::reactive::Flux;
    use firefly::web::{NdJson, Sse};

    // `load_events` returns `Err(NotFound)` for an absent wallet, so the 404 is
    // decided before the streaming response head is committed.
    let events = match api.ledger.load_events(&id).await {
        Ok(events) => events,
        Err(e) => return WebError::from(domain_to_web(e)).into_response(),
    };
    let items: Vec<WalletEvent> = events.iter().map(WalletEvent::from_domain).collect();
    let flux = Flux::just(items);
    if params.format.as_deref() == Some("sse") {
        Sse(flux).into_response()
    } else {
        NdJson(flux).into_response()
    }
}
```

What just happened, block by block:

- `streaming_router` returns a plain `axum::Router` with `GET
  /api/v1/wallets/:id/events` mapped to `stream_events` and the `WalletApi` state
  attached. This is the sub-router `StreamingRoutes::routes` hands back.
- `stream_events` first calls `api.ledger.load_events(&id)`. If the wallet is
  absent the ledger returns `Err(NotFound)`, and the handler renders that as an
  RFC 9457 `application/problem+json` 404 *and returns* — before any streaming
  body has started.
- On success it maps the domain events to the `WalletEvent` view shape, wraps the
  `Vec` in `Flux::just(...)`, and chooses the encoding: `Sse(flux)` for
  `?format=sse`, otherwise `NdJson(flux)`.

> **Note** **Key term — `NdJson` and `Sse`.** `NdJson(flux)` (`pub struct
> NdJson<T>(pub Flux<T>)`) renders the `Flux` as one JSON document per line with
> content type `application/x-ndjson`; `Sse(flux)` renders Server-Sent Events with
> content type `text/event-stream`. Both wrap a `Flux<T>` and implement
> `IntoResponse`, so a handler returns them directly.

> **Warning** Order matters here. The 404 for an unknown wallet must be resolved
> *before* the response head is committed, because once a streaming body starts
> the status line is already on the wire and can no longer change. That is why
> `load_events` is awaited and checked first, and only then is a `Flux` built.

> **Note** `Flux::just(items)` materializes a known `Vec` — fine for a finite
> event history that is already loaded. A production stream over a live, unbounded
> source (e.g. a broker subscription) would use `Flux::from_stream(...)` instead,
> so the body is produced lazily with backpressure rather than buffered up front.

### 3c — Prove the three behaviors with a test

`src/streaming_test.rs` (compiled only under `#[cfg(all(test, feature =
"streaming"))]`) boots one app context, opens a wallet, makes a deposit — so the
stream has two events — and asserts the NDJSON default, the SSE switch, and the
404. The default case:

```rust,ignore
// src/streaming_test.rs
#[tokio::test]
async fn events_stream_as_ndjson_by_default() {
    let app = build_router().await;
    let id = open_with_deposit(&app).await; // two events: WalletOpened + MoneyDeposited
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/wallets/{id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(ct.contains("ndjson"), "default stream should be NDJSON, got {ct:?}");

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected 2 NDJSON lines, got: {text:?}");
    assert!(text.contains("WalletOpened"));
    assert!(text.contains("MoneyDeposited"));
}
```

What just happened: `build_router().await` returns the fully wired public router
in-process (it calls `FireflyApplication::bootstrap()` under the hood, as in
[Testing](./18-testing.md)). The test drives it with `tower::ServiceExt::oneshot`
— no socket bound — opens a wallet with a deposit, then `GET`s the events stream
and asserts the response is `200`, is `application/x-ndjson`, and carries exactly
two JSON lines (the `WalletOpened` and the `MoneyDeposited`). Sibling tests assert
that `?format=sse` flips the content type to `text/event-stream` and that an
unknown wallet id is a `404`.

> **Tip** **Checkpoint.** Build and test with the feature on:
>
> ```bash
> cargo test -p firefly-sample-lumen --features streaming
> ```
>
> All three streaming tests pass. Then run the binary the same way —
> `cargo run --bin lumen --features streaming` — open a wallet, deposit, and
> `curl http://127.0.0.1:8080/api/v1/wallets/<id>/events` to see the two NDJSON
> lines on the public port.

## Step 4 — Split the management surface for production

Always serve the actuator on a **different listener** from the public API so
`/actuator/*` is reachable by your orchestrator but never on the public network —
exactly the `:8080` / `:8081` split Lumen uses by default. Firewall the management
port to your cluster's internal network. The health sub-paths feed your
orchestrator's probes:

| Probe     | Endpoint                            |
|-----------|-------------------------------------|
| liveness  | `/actuator/health/liveness`         |
| readiness | `/actuator/health/readiness`        |
| overall   | `/actuator/health`                  |

What just happened: the management router (Step 2, stage 7) mounts the full
actuator tree plus the admin dashboard and the API docs. The liveness probe
reports only indicators tagged for liveness (is the process alive?); readiness
reports only readiness indicators (can it serve traffic — dependencies up?). Point
your orchestrator's liveness probe at `/actuator/health/liveness` and its
readiness probe at `/actuator/health/readiness`, both on `:8081`.

> **Note** Metrics for scraping live on the same management port:
> `/actuator/prometheus` serves labeled Prometheus exposition, and
> `/actuator/metrics` serves the JSON view. Point your scraper at `:8081`,
> never `:8080`.

> **Tip** **Checkpoint.** With Lumen running, from a second terminal:
> `curl localhost:8081/actuator/health/readiness` returns a JSON body with a
> `"status"`, and the same path on `:8080` returns nothing — the public port has
> no `/actuator/*`.

## Step 5 — Turn on production-hardening middleware

The framework already turns on the web-tier batteries when `FireflyApplication`
builds the web stack: the RFC 9457 problem renderer, correlation-id propagation,
W3C trace-context origination, request metrics, and idempotency replay. The
remaining production middleware is opt-in through `CoreConfig`, tuned via
`FireflyApplication::configure(|cfg| { … })`, and each knob weaves its layer in at
the correct filter order:

```rust,ignore
// Opt-in production middleware, tuned at the entry point.
firefly::FireflyApplication::new("lumen")
    .configure(|cfg| {
        cfg.cors = Some(firefly::web::CorsConfig::default());
        cfg.security_headers = Some(firefly::web::SecurityHeadersConfig::default());
        cfg.csrf = Some(firefly::web::CsrfLayer::new()); // browser flows only
        cfg.request_log = Some(firefly::web::RequestLogLayer::default());
    })
    .run()
    .await
```

The knobs and what each adds:

| Knob               | Adds                                                  |
|--------------------|-------------------------------------------------------|
| `cors`             | CORS preflight + simple-request decoration            |
| `security_headers` | OWASP response headers (`nosniff`, `DENY`, HSTS, …)   |
| `csrf`             | double-submit-cookie CSRF (for browser flows)         |
| `request_log`      | one structured access-log event per request           |
| `request_metrics`  | `http_server_requests_seconds` + `_max` (actuator)    |
| `http_exchanges`   | recent-exchange recorder + `/actuator/httpexchanges`  |
| `loggers`          | `/actuator/loggers` runtime log-level control         |

What just happened: `configure(|cfg| { … })` hands you the `CoreConfig` before the
web stack is built, so the layers you switch on are woven in at boot. Every
optional knob defaults to OFF, except request metrics, which are on by default
(Spring-Boot-style auto-instrumentation) and tuned — or disabled — through
`request_metrics` / `disable_request_metrics`.

The effective chain, outermost (nearest the network) to innermost (nearest your
handler), is:

```text
CorsLayer            (cors)              — preflight + simple-request edge
ProblemLayer         (always)           — panic → RFC 9457 500
SecurityHeadersLayer (security_headers) — decorate every response
TraceContextLayer    (always)           — validate/originate W3C traceparent
CorrelationLayer     (always)           — X-Correlation-Id (+ request ctx)
MetricsLayer         (request_metrics)  — http_server_requests_*
HttpExchangesLayer   (http_exchanges)   — record into the recorder
RequestLogLayer      (request_log)      — one access-log event
CsrfLayer            (csrf)             — double-submit cookie
IdempotencyLayer     (always)           — replay on Idempotency-Key
        │
        ▼
     your router
```

Idempotency stays innermost so a replayed request still passes every outer
concern (correlation, metrics, the access log). The W3C `TraceContextLayer` sits
just outside correlation so it can originate a root span and a `traceparent` that
the inner correlation layer then echoes on the response.

> **Design note.** This is the same actuator-and-middleware posture Spring Boot
> ships, but switched on declaratively at one call site rather than through a
> scatter of properties and `@Configuration` classes. A bare `FireflyApplication`
> already gives you the always-on Problem → TraceContext → Correlation →
> Idempotency core; `configure(...)` adds the rest.

> **Tip** **Checkpoint.** Run Lumen with `security_headers` on and
> `curl -i localhost:8080/api/v1/wallets/anything`. The response carries
> `X-Content-Type-Options: nosniff` and `X-Frame-Options: DENY` even on the 404
> problem body — proof the layer decorates every response, including recovered
> errors.

## Step 6 — Configure for production from the environment

Bind configuration from layered sources with environment overrides on top, so a
container reads its settings from the environment ([Configuration](./03-configuration.md)).
For Lumen the two bind addresses are already env-driven, so the production
container needs no config file just to move its ports:

```bash
FIREFLY_PROFILE=prod \
FIREFLY_SERVER_ADDR=0.0.0.0:8080 \
FIREFLY_MANAGEMENT_ADDR=0.0.0.0:8081 \
  ./lumen
```

What just happened: `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` are read at
construction time (Step 2) and override the `0.0.0.0:8080` / `0.0.0.0:8081`
defaults, while `FIREFLY_PROFILE=prod` selects the production property layer.
`FIREFLY_*` variables beat the YAML files, secrets are masked in `/actuator/env`,
and `${...}` placeholders resolve env-then-config-then-default.

> **Warning** The JWT signing key from [Security](./14-security.md) is the obvious
> thing to inject this way — from the environment or a secret store — rather than
> the inline `DEMO_SIGNING_KEY` constant Lumen ships for teaching. Never bake a
> real signing key into the binary or commit it to source.

## Step 7 — Swap in-memory infrastructure for Postgres and Kafka

This is the payoff of the whole architecture. Lumen swaps its in-memory defaults
for real backends by **changing a `#[bean]` factory in `LumenBeans`**, and nothing
downstream changes. Recall the in-memory factory in `src/web.rs`:

```rust,ignore
// src/web.rs — today
#[bean]
impl LumenBeans {
    /// The in-memory event store (`@Bean`).
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }

    // … the ledger factory autowires the EventStore + the framework Broker port
    #[bean]
    fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

To go to Postgres, return the framework's SQL-backed event store
(`firefly::eventsourcing::SqlEventStore`) behind the same `EventStore` port. It
takes a `Database` port, so the swap is contained to the factory:

```rust,ignore
// src/web.rs — production: a Postgres-backed event store behind the EventStore port.
use firefly::eventsourcing::{EventStore, SqlEventStore};

#[bean]
impl LumenBeans {
    /// An async `#[bean]` factory: connect the pool, build the SQL event store,
    /// create its table once, and hand back a `dyn EventStore` for the `ledger`
    /// factory to autowire. Any error here aborts startup (fail-fast).
    #[bean]
    async fn event_store(&self, db: Arc<dyn firefly::transactional::Database>) -> SqlEventStore {
        let store = SqlEventStore::new(db);
        store.initialize().expect("create event-store table");
        store
    }
}
```

What just happened: the `ledger` factory depends on the `EventStore` *port*, and
the `Ledger`, the read-model projection, the CQRS handlers, the saga, the TCC
transfer, and every test are written against the `EventStore` and `Broker` ports —
so the wallet domain never learns it moved from a `HashMap` to Postgres. The same
shape applies to messaging: where Lumen overrides the broker, a `#[bean]` returns
a Kafka adapter behind the framework's `Broker` port, and the EDA projection
listener consumes from Kafka instead of the in-process bus without changing a line
of the projection.

> **Note** The `event_store` bean here is an `async fn`. The framework awaits async
> bean factories during the component-scan (Step 2, stage 2), so the pool is dialed
> and the store is live before anything resolves it — and a connection failure
> aborts startup rather than surfacing on the first request. That is the fail-fast
> property you want in production.

> **Design note.** "Swap the adapter, keep the code" applied to the storage and
> messaging tiers, with the swap localized to one bean factory. The domain, the
> handlers, the projection, the saga, and the tests are written against ports —
> exactly the hexagonal design this book has built toward.

> **Tip** **Checkpoint.** You do not need to actually stand up Postgres to learn
> the shape: read the `ledger` factory and confirm it names `Arc<dyn EventStore>`,
> not `MemoryEventStore`. Anything that autowires the *port* is swap-ready by
> construction.

## Step 8 — Package Lumen as a container

A typical multi-stage build compiles the release binary in a Rust image, then
copies just the binary into a slim runtime image:

```dockerfile
# Dockerfile
FROM rust:1.88 AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p firefly-sample-lumen

FROM debian:bookworm-slim
COPY --from=build /app/target/release/lumen /usr/local/bin/lumen
EXPOSE 8080 8081
ENTRYPOINT ["/usr/local/bin/lumen"]
```

What just happened: the `build` stage compiles the `firefly-sample-lumen` package
(its `[[bin]]` is named `lumen`, so the artifact lands at
`target/release/lumen`); the runtime stage copies only that binary onto a minimal
Debian image and exposes both ports. Because `run()` traps SIGTERM and drains
(Step 2), the container stops cleanly when the orchestrator sends a termination
signal — no `--init` shim or signal-forwarding wrapper required.

> **Tip** **Checkpoint.** `docker build -t lumen .` produces an image, and
> `docker run -p 8080:8080 -p 8081:8081 lumen` boots it. `docker stop` on that
> container exits gracefully (no SIGKILL fallback within the drain budget),
> because the binary handles SIGTERM itself.

## A deployment checklist

- [ ] Actuator (`:8081`) on a **separate, firewalled** port from the API (`:8080`).
- [ ] Liveness/readiness probes pointed at
      `/actuator/health/{liveness,readiness}`.
- [ ] `security_headers`, `cors`, and (for browser flows) `csrf` switched on
      through `configure(...)`.
- [ ] `request_log` + `request_metrics` on; logs shipped as JSON, metrics scraped
      from `/actuator/prometheus`.
- [ ] Correlation propagation verified end-to-end across services.
- [ ] JWT signing key injected from the environment / a secret store, not inline.
- [ ] In-memory event store + broker swapped for Postgres + Kafka in the
      `LumenBeans` `#[bean]` factories.
- [ ] Graceful-shutdown drain budget tuned for the slowest in-flight transfer.
- [ ] The verification gate green: `cargo test -p firefly-sample-lumen` and
      `--features streaming`, plus `clippy -D warnings` and `fmt --check`.

## Recap — what changed in Lumen

| Before this chapter | After this chapter |
|---------------------|--------------------|
| a wired service you ran but had not taken to production | a deployable container with the management split, hardening middleware, and a port-swap path |
| no streaming endpoint | the feature-gated `StreamingRoutes` `RouteContributor` bean serving `GET /api/v1/wallets/:id/events` as NDJSON (default) or SSE (`?format=sse`) |
| in-memory event store / broker only | the one-`#[bean]` swap to `SqlEventStore` + a Kafka `Broker` adapter, with nothing downstream changed |

You also now know:

- That `run()` is `bootstrap().await?.serve().await`: an eight-stage boot, two
  servers each with its own drain, and a *cancelled* error mapped to `Ok(())` so a
  signalled shutdown exits zero.
- That a feature-gated endpoint is wired purely by declaring a `RouteContributor`
  bean — `main` never changes — and that a streaming handler must resolve its 404
  before the response head is committed.
- That production hardening (CORS, OWASP headers, CSRF, access log) is opt-in
  through `CoreConfig` at one `configure(...)` call site, weaving into the correct
  filter order.
- That the storage and messaging swap is one bean factory, because the domain,
  handlers, projection, saga, and tests all depend on the `EventStore` and
  `Broker` ports.

That completes the guided build arc. Lumen began as an empty directory in
[Quickstart](./02-quickstart.md); it is now a secure, observable, event-sourced
CQRS service that streams its history and deploys as a single container — and the
one-line `main` never changed.

## Exercises

1. **Run and drain.** `cargo run --bin lumen`, open a wallet, then `Ctrl-C` and
   watch the graceful drain. Confirm the process exits zero with `echo $?`, and
   that no stack trace prints — the cancelled-to-`Ok(())` mapping from Step 2.
2. **Override the ports.** Start Lumen with `FIREFLY_SERVER_ADDR=0.0.0.0:9000
   FIREFLY_MANAGEMENT_ADDR=0.0.0.0:9001 cargo run --bin lumen` and confirm the API
   moved to `:9000` and the actuator to `:9001`, independently.
3. **Stream the history.** Build with `--features streaming`, open a wallet and
   deposit, then `curl http://127.0.0.1:8080/api/v1/wallets/<id>/events` (NDJSON)
   and `…?format=sse` (SSE). Compare the two `content-type` headers, and confirm
   `GET /api/v1/wallets/wlt_missing/events` returns a `404` problem document.
4. **Harden the chain.** Add `cfg.security_headers = Some(...)` and
   `cfg.request_log = Some(...)` through `configure(...)`, re-run, and inspect a
   response with `curl -i`. Find the OWASP headers, then place each layer in the
   outermost-to-innermost chain from Step 5.
5. **Sketch the Postgres swap.** Write the `event_store` `#[bean]` in `LumenBeans`
   that returns a `SqlEventStore` over a `Database` port, and explain in one
   sentence why the `ledger` factory, the read-model projection, and the tests
   need no change.

## Where to go next

- Revisit the declarative macros that made all of this possible — the `#[bean]`,
  `#[rest_controller]`, `#[command_handler]`, `#[saga]`, and `#[scheduled]`
  attributes as a capstone — in
  **[Declarative Services with Macros](./21-declarative-macros.md)**.
- Look up any building block by crate in the
  **[Module Index](./91-appendix-modules.md)**, or any term in the
  **[Glossary](./92-glossary.md)**.
