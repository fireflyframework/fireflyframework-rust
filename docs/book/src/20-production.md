# Production & Deployment

Lumen has grown from a bare scaffold into a secure, observable, event-sourced
CQRS service with a saga and a scheduled task. This last chapter of the build arc
looks at how the **one-line `main`** boots and runs reliably in production, and
turns on the optional **reactive streaming endpoint**. It also covers everything
between "it works on my machine" and a service in production: graceful shutdown,
the management split, configuration, packaging, and the swap from in-memory
infrastructure to real Postgres + Kafka.

By the end of this chapter you will understand Lumen's boot pipeline end to end:
two servers (public API + management) with graceful SIGINT/SIGTERM shutdown, and
the streaming endpoint (`GET /api/v1/wallets/:id/events` → NDJSON / SSE) wired as
a `RouteContributor` bean behind the `streaming` feature.

> **The one-line `main`.** Lumen's whole entry point is
> `firefly::FireflyApplication::new("lumen").run().await`. `run()` is the process
> supervisor: it builds and serves the public + management ports, traps
> SIGINT/SIGTERM, gives each server its own drain signal, and exits cleanly once
> in-flight work drains. A streaming endpoint returns a `Flux<T>` as NDJSON or SSE.

## The boot pipeline and graceful shutdown

There is no hand-written lifecycle wiring in Lumen — `run()` does it all. Under
the hood, `FireflyApplication::bootstrap()` assembles the app and
`Bootstrapped::serve()` runs it on the lifecycle `Application`, which traps
SIGINT/SIGTERM, gives each server task its own drain signal, and grants a drain
budget before exiting. The pipeline, in order:

1. **Build the web stack** and tee logging into the admin capture buffer.
2. **Component-scan the container** — auto-register the framework's infra beans,
   then discover Lumen's `#[derive(Configuration)]`/`#[bean]`,
   `#[derive(Controller)]`, and `#[autowired]` beans.
3. **Auto-configure the CQRS bus** — correlation always; the read-cache
   middleware because Lumen declares a `QueryCache` bean.
4. **Auto-discover security** — the `FilterChain` + `BearerLayer` beans
   (Chapter 14), layered onto the API with no `.security(...)` call.
5. **Auto-mount controllers** — every `#[rest_controller]` and every
   `RouteContributor` bean (including the streaming endpoint below), then apply
   the middleware chain + W3C trace origination.
6. **Drain the discovered handlers** — the CQRS handlers, the EDA listener, and
   the `#[scheduled]` housekeeping task, from the inventory registries.
7. **Self-host the admin dashboard** on the management port and auto-serve the
   OpenAPI docs.
8. **Print the startup report**, then **serve both ports** with graceful drain.

The two servers and the drain are the part that matters for production:

- **Two servers, two drains.** The public API serves on `:8080` and the
  management surface (`/actuator/*` + the self-hosted `/admin` dashboard) on
  `:8081`. Each runs on its own task with its own `shutdown` handle, so a signal
  drains both listeners independently.
- **`run()` blocks until a signal.** It returns when SIGINT/SIGTERM is received
  and the drain completes. A clean shutdown surfaces as a *cancelled* error,
  which `run()` itself maps to `Ok(())`; any other error propagates out of `main`.
- **Env-overridable binds.** `FIREFLY_SERVER_ADDR` and `FIREFLY_MANAGEMENT_ADDR`
  override the defaults (`0.0.0.0:8080` / `0.0.0.0:8081`), so a container reads
  its ports from the environment.

## The full boot sequence

Read top to bottom, `main()` is one line — the framework does the six-step boot
for you:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

That single `run()` call component-scans Lumen's beans — the CQRS bus, the
event-sourced ledger, the read model, the projection (seeded inside the `ledger`
`#[bean]`), and the security chain over in-memory infrastructure — auto-mounts
the controllers, drains the discovered handlers/listener/`#[scheduled]` task,
self-hosts the admin dashboard, prints the banner and the line-by-line startup
report, and serves both ports. Everything you have seen chapter by chapter is
*declared as a bean*; `main` just hands the crate to the framework.

> **Teaching code runs with no dependencies.** Lumen's binary boots with an
> in-memory event store and broker, so `cargo run --bin lumen` needs nothing
> external. The tests drive `build_router()` (which calls
> `FireflyApplication::bootstrap()`) in-process rather than this binary.

## The reactive streaming endpoint (feature `streaming`)

Lumen's last endpoint streams a wallet's event history. It is feature-gated so
the teaching baseline stays lean — `Cargo.toml` declares it, and it needs nothing
beyond the facade (`firefly::reactive::Flux` + `firefly::web::{NdJson, Sse}`):

```toml
[features]
default = []
streaming = []
```

The endpoint is wired by **declaring a bean**, not by editing an entry point. A
`#[derive(Service)]` that `provides = "dyn firefly::web::RouteContributor"`
contributes the sub-router; `FireflyApplication` resolves it as the
`dyn RouteContributor` port and merges its routes, so a feature-gated endpoint
appears purely because its crate compiled it in:

```rust,ignore
/// (feature `streaming`) A `RouteContributor` bean adding the reactive
/// `GET /api/v1/wallets/:id/events` endpoint. The framework discovers it and
/// merges its routes — no composition-root step.
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

The handler itself lives on the sub-router `streaming_router` builds. It loads
the wallet's persisted events, maps them to the view shape, wraps them in a
`Flux`, and returns NDJSON by default or SSE when `?format=sse` is passed:

```rust,ignore
async fn stream_events(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<StreamParams>,
) -> Response {
    use crate::domain::WalletEvent;
    use axum::response::Response;
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

`NdJson(flux)` renders one JSON document per line (`application/x-ndjson`);
`Sse(flux)` renders Server-Sent Events (`text/event-stream`). The 404 for an
unknown wallet is resolved *before* the response head is committed, because once
a streaming body starts you can no longer change the status. `tests/streaming.rs`
(run with `--features streaming`) proves all three behaviors:

```rust,ignore
#[tokio::test]
async fn events_stream_as_ndjson_by_default() {
    let app = build_router().await;
    let id = open_with_deposit(&app).await;          // two events: opened + deposited
    let res = app
        .clone()
        .oneshot(Request::get(format!("/api/v1/wallets/{id}/events")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // content-type contains "ndjson"; the body is two JSON lines:
    // one "WalletOpened", one "MoneyDeposited".
}
```

> **Streaming responses.** Returning a `Flux<T>` as `application/x-ndjson` or
> `text/event-stream` streams element-by-element with backpressure. `Flux::just`
> materializes a known `Vec`; a production stream would use `Flux::from_stream`
> over a live subscription so the body is produced lazily rather than buffered.

## The management split in production

Always serve the actuator on a **different listener** from the public API so
`/actuator/*` is reachable by your orchestrator but never on the public network —
exactly the `:8080` / `:8081` split Lumen uses. Firewall the admin port. The
health sub-paths feed your orchestrator's probes:

| Probe     | Endpoint                            |
|-----------|-------------------------------------|
| liveness  | `/actuator/health/liveness`         |
| readiness | `/actuator/health/readiness`        |
| overall   | `/actuator/health`                  |

## Production hardening middleware

The framework already turns on CORS, OWASP security headers, request metrics,
and the access log (the web-tier batteries) when `FireflyApplication` builds the
web stack. The remaining production middleware is opt-in through `CoreConfig` —
tuned via `FireflyApplication::configure(|cfg| { … })` — weaving in at the
correct filter order:

| Knob               | Adds                                                  |
|--------------------|-------------------------------------------------------|
| `cors`             | CORS preflight + simple-request decoration            |
| `security_headers` | OWASP response headers (`nosniff`, `DENY`, HSTS, …)   |
| `csrf`             | double-submit-cookie CSRF (for browser flows)         |
| `request_log`      | one structured access-log event per request           |
| `request_metrics`  | `http_server_requests_seconds` + `_max` (actuator)    |
| `http_exchanges`   | recent-exchange recorder + `/actuator/httpexchanges`  |
| `loggers`          | `/actuator/loggers` runtime log-level control          |

The effective chain (outermost → innermost) is CORS → Problem → SecurityHeaders →
Correlation → Metrics → HttpExchanges → RequestLog → CSRF → Idempotency → your
router. Idempotency stays innermost so a replayed request still passes every
outer concern.

## Configuration in production

Bind configuration from layered sources with environment overrides on top, so a
container reads its settings from the environment (Chapter 3). For Lumen, the two
bind addresses are already env-driven:

```bash
FIREFLY_PROFILE=prod \
FIREFLY_SERVER_ADDR=0.0.0.0:8080 \
FIREFLY_MANAGEMENT_ADDR=0.0.0.0:8081 \
  ./lumen
```

`FIREFLY_*` variables beat the YAML files, secrets are masked in `/actuator/env`,
and `${...}` placeholders resolve env-then-config-then-default. The JWT signing
key (Chapter 14) is the obvious thing to inject this way rather than inline.

## From in-memory to real infrastructure

This is the payoff of the whole architecture: Lumen swaps its in-memory defaults
for real backends by **changing a `#[bean]` factory in `LumenBeans`**, and
nothing downstream changes. The `event_store` bean returns a `MemoryEventStore`
today; production returns a durable store instead, and (where Lumen overrides
the broker) a `#[bean]` returns a Kafka adapter behind the `Broker` port:

```rust,ignore
#[bean]
impl LumenBeans {
    // today:
    #[bean]
    fn event_store(&self) -> MemoryEventStore { MemoryEventStore::new() }

    // production: a Postgres-backed event store behind the EventStore port.
    // #[bean]
    // fn event_store(&self) -> PostgresEventStore { PostgresEventStore::connect(...) }
}
```

The `ledger` factory depends on the `EventStore` *port*, and the `Ledger`, the
projection, the CQRS handlers, the saga, and every test are written against the
`EventStore` and `Broker` ports — so the wallet domain never learns it moved
from a `HashMap` to Postgres + Kafka. That is "swap the adapter, keep the code,"
applied to the storage and messaging tiers, with the swap localized to one bean
factory.

## Container packaging

A typical multi-stage build for Lumen:

```dockerfile
FROM rust:1.88 AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p firefly-sample-lumen

FROM debian:bookworm-slim
COPY --from=build /app/target/release/lumen /usr/local/bin/lumen
EXPOSE 8080 8081
ENTRYPOINT ["/usr/local/bin/lumen"]
```

Because `run()` traps SIGTERM and drains, the container stops cleanly when the
orchestrator sends a termination signal — no `--init` shim or signal-forwarding
wrapper required.

## A deployment checklist

- [ ] Actuator (`:8081`) on a **separate, firewalled** port from the API (`:8080`).
- [ ] Liveness/readiness probes pointed at `/actuator/health/{liveness,readiness}`.
- [ ] `security_headers`, `cors`, and (for browser flows) `csrf` on.
- [ ] `request_log` + `request_metrics` on; logs shipped as JSON, metrics scraped
      from `/actuator/prometheus`.
- [ ] Correlation propagation verified end-to-end across services.
- [ ] JWT signing key injected from the environment / a secret store, not inline.
- [ ] In-memory event store + broker swapped for Postgres + Kafka in the
      `LumenBeans` `#[bean]` factories.
- [ ] Graceful-shutdown drain budget tuned for the slowest in-flight transfer.
- [ ] The verification gate green: `cargo test -p firefly-sample-lumen` and
      `--features streaming`, plus `clippy -D warnings` and `fmt --check`.

## What changed in Lumen

This chapter looked at the boot pipeline and added the optional stream — the
last code the arc adds:

- **`src/main.rs`** is one line: `FireflyApplication::new("lumen").run().await`.
  `run()` component-scans the beans, auto-mounts the controllers, drains the
  discovered handlers/listener/`#[scheduled]` task, self-hosts the admin
  dashboard, prints the startup report, and serves the public + management ports
  with graceful SIGINT/SIGTERM drain. A clean shutdown is a *cancelled* error
  that `run()` maps to `Ok(())`.
- **`Cargo.toml`** declares the `streaming` feature; **`src/web.rs`** adds the
  feature-gated `StreamingRoutes` `RouteContributor` bean (and its
  `stream_events` handler) that returns a `Flux<WalletEvent>` as NDJSON (default)
  or SSE (`?format=sse`), with the 404 resolved before the response head — wired
  purely by declaring the bean.
- **`tests/streaming.rs`** proves the NDJSON default, the SSE switch, and the
  404, all behind the `streaming` feature.
- The chapter showed the **in-memory → Postgres + Kafka** swap as a one-bean
  change in `LumenBeans`, proving the port-and-adapter design end to end.

## Exercises

1. **Run and drain.** `cargo run --bin lumen`, open a wallet, then Ctrl-C and
   watch the graceful drain. Confirm the process exits zero.
2. **Override the ports.** Start Lumen with `FIREFLY_SERVER_ADDR=0.0.0.0:9000
   FIREFLY_MANAGEMENT_ADDR=0.0.0.0:9001 cargo run --bin lumen` and confirm the API
   and management surfaces move.
3. **Stream the history.** Build with `--features streaming`, open a wallet and
   deposit, then `curl http://127.0.0.1:8080/api/v1/wallets/<id>/events` (NDJSON)
   and `...?format=sse` (SSE). Compare the two content types.
4. **Sketch the Postgres swap.** Write the `event_store` `#[bean]` in `LumenBeans`
   that would return a Postgres-backed `EventStore`, and explain in one sentence
   why the `ledger` factory, the projection, and the tests need no change.

That completes the guided tour of Lumen. The remaining chapters revisit the
declarative macros as a capstone and provide reference material: the
[Module Index](./91-appendix-modules.md) and the [Glossary](./92-glossary.md).
