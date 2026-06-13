# Production & Deployment

Lumen has grown from a bare scaffold into a secure, observable, event-sourced
CQRS service with a saga and a scheduled task. This last chapter of the build arc
wires the **process entry point** — the thing that boots Lumen and keeps it
running reliably — and turns on the optional **reactive streaming endpoint**. It
also covers everything between "it works on my machine" and a service in
production: graceful shutdown, the management split, configuration, packaging,
and the swap from in-memory infrastructure to real Postgres + Kafka.

By the end of this chapter you will have read Lumen's `main.rs` end to end: a
banner, two servers (public API + actuator) wired through the lifecycle
`Application`, graceful SIGINT/SIGTERM shutdown, and the streaming endpoint
(`GET /api/v1/wallets/:id/events` → NDJSON / SSE) behind the `streaming` feature.

> **Spring parity.** The lifecycle `Application` is `SpringApplication.run()`: it
> traps the termination signals, drains in-flight work, and runs lifecycle hooks.
> Running the actuator on a second port is the Spring Boot
> `management.server.port` split. The reactive endpoint is WebFlux's streaming
> `Flux<T>` response.

## The lifecycle and graceful shutdown

`firefly-lifecycle`'s `Application` traps SIGINT/SIGTERM, gives each server task
its own drain signal, and grants a drain budget before exiting.
`Core::new_application()` (reachable through the `WebStack`'s `Deref`) builds one
named after the app. This is the spine of Lumen's `src/main.rs`:

```rust,ignore
let api_addr = std::env::var("LUMEN_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_owned());
let admin_addr =
    std::env::var("LUMEN_ADMIN_ADDR").unwrap_or_else(|_| DEFAULT_ADMIN_ADDR.to_owned());

let application = app
    .web
    .new_application()
    .on_server("api", move |shutdown| async move {
        let listener = tokio::net::TcpListener::bind(&api_addr).await?;
        axum::serve(listener, api)
            .with_graceful_shutdown(shutdown.wait())
            .await?;
        Ok(())
    })
    .on_server("admin", move |shutdown| async move {
        let listener = tokio::net::TcpListener::bind(&admin_addr).await?;
        axum::serve(listener, admin)
            .with_graceful_shutdown(shutdown.wait())
            .await?;
        Ok(())
    });

if let Err(err) = application.run().await {
    if !err.is_cancelled() {
        eprintln!("application failed: {err}");
        std::process::exit(1);
    }
}
```

The key moves:

- **Two servers, two drains.** `on_server("api", ..)` and `on_server("admin", ..)`
  register the public API on `:8080` and the actuator on `:8081`. Each closure
  receives its own `shutdown` handle; passing `shutdown.wait()` to
  `axum::serve(...).with_graceful_shutdown` drains that listener when the signal
  arrives. Multiple servers are allowed, each on its own task.
- **`run()` blocks until a signal.** It returns when SIGINT/SIGTERM is received
  and the drain completes. A clean shutdown surfaces as a *cancelled* error —
  `err.is_cancelled()` — which Lumen treats as success; any other error exits
  non-zero. (`on_start` / `on_stop` hooks and `app.shutdown_handle()` for
  programmatic shutdown round out the API; Lumen needs neither.)
- **Env-overridable binds.** `LUMEN_ADDR` and `LUMEN_ADMIN_ADDR` override the
  defaults, so a container reads its ports from the environment.

## The full boot sequence

Read top to bottom, `main()` does exactly six things — build, log, assemble the
two routers, start the scheduler, print the banner, run:

```rust,ignore
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = build_app().await;
    // Best-effort: a test harness may already own the global subscriber.
    let _ = app.web.init_logging();

    let api = app.router();
    let contributor: InfoContributor = Box::new(|| {
        let mut info = serde_json::Map::new();
        info.insert(
            "sample".into(),
            serde_json::json!({ "name": APP_NAME, "store": "in-memory", "eventBus": "in-memory" }),
        );
        info
    });
    let admin = app.web.actuator_router(vec![contributor]);

    // Register and start the scheduled housekeeping task on a background task.
    let scheduler = build_scheduler();
    tokio::spawn(async move { scheduler.start().await });

    app.web.print_banner();
    println!(":: {APP_NAME} :: digital-wallet & ledger (v{VERSION})");

    // ... the lifecycle Application from the previous section ...
}
```

`build_app()` is the composition root from Chapter 4 — it wires the
`WebStack`, the CQRS bus, the event-sourced ledger, the read model, the
projection, and the security chain over in-memory infrastructure.
`app.router()` (Chapter 14) adds the JWT bearer + RBAC layers. `print_banner()`
emits the ASCII Firefly banner and the version tagline. Everything else you have
already seen, chapter by chapter — `main.rs` just assembles it.

> **Teaching code runs with no dependencies.** Lumen's binary boots with an
> in-memory event store and broker, so `cargo run --bin lumen` needs nothing
> external. The tests drive `build_router()` in-process rather than this binary.

## The reactive streaming endpoint (feature `streaming`)

Lumen's last endpoint streams a wallet's event history. It is feature-gated so
the teaching baseline stays lean — `Cargo.toml` declares it, and it needs nothing
beyond the facade (`firefly::reactive::Flux` + `firefly::web::{NdJson, Sse}`):

```toml
[features]
default = []
streaming = []
```

The handler lives on a separate, feature-gated sub-router merged into
`LumenApp::router`, so the macro-generated `routes()` never references a method
that is compiled out. It loads the wallet's persisted events, maps them to the
view shape, wraps them in a `Flux`, and returns NDJSON by default or SSE when
`?format=sse` is passed:

```rust,ignore
async fn stream_events(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<StreamParams>,
) -> Response {
    use crate::domain::WalletEvent;
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
    let id = open_with_deposit().await;          // two events: opened + deposited
    let res = build_router()
        .await
        .oneshot(Request::get(format!("/api/v1/wallets/{id}/events")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // content-type contains "ndjson"; the body is two JSON lines:
    // one "WalletOpened", one "MoneyDeposited".
}
```

> **Spring parity.** Returning a `Flux<T>` as `application/x-ndjson` or
> `text/event-stream` is exactly WebFlux's streaming response. `Flux::just` is
> Reactor's `Flux.just`; a production stream would use `Flux::from_stream` over a
> live subscription instead of a materialized `Vec`.

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

Lumen's `WebStack::new` already turns on CORS, OWASP security headers, request
metrics, and the access log (the web-tier batteries). The remaining
pyfly-parity middleware is opt-in through `CoreConfig`, weaving in at the correct
filter order:

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
LUMEN_ADDR=0.0.0.0:8080 \
LUMEN_ADMIN_ADDR=0.0.0.0:8081 \
  ./lumen
```

`FIREFLY_*` variables beat the YAML files, secrets are masked in `/actuator/env`,
and `${...}` placeholders resolve env-then-config-then-default. The JWT signing
key (Chapter 14) is the obvious thing to inject this way rather than inline.

## From in-memory to real infrastructure

This is the payoff of the whole architecture: Lumen swaps its in-memory defaults
for real backends **at the composition root**, and nothing downstream changes.
`build_app` constructs a `MemoryEventStore` and reads the `WebStack`'s in-memory
broker; production replaces those two lines:

```rust,ignore
// build_app() today:
let store: Arc<dyn firefly::eventsourcing::EventStore> = Arc::new(MemoryEventStore::new());
let broker = Arc::clone(&web.broker);

// production: a Postgres-backed event store and a Kafka broker, same ports.
// let store: Arc<dyn EventStore> = Arc::new(postgres_event_store);  // firefly-eda-postgres
// let broker = Arc::new(kafka_broker);                              // firefly-eda-kafka
```

The `Ledger`, the projection, the CQRS handlers, the saga, and every test are
written against the `EventStore` and `Broker` *ports* — so the wallet domain
never learns it moved from a `HashMap` to Postgres + Kafka. That is "swap the
adapter, keep the code," applied to the storage and messaging tiers.

## Container packaging

A typical multi-stage build for Lumen:

```dockerfile
FROM rust:1.85 AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p firefly-sample-lumen

FROM debian:bookworm-slim
COPY --from=build /app/target/release/lumen /usr/local/bin/lumen
EXPOSE 8080 8081
ENTRYPOINT ["/usr/local/bin/lumen"]
```

Because the lifecycle `Application` traps SIGTERM and drains, the container stops
cleanly when the orchestrator sends a termination signal — no `--init` shim or
signal-forwarding wrapper required.

## A deployment checklist

- [ ] Actuator (`:8081`) on a **separate, firewalled** port from the API (`:8080`).
- [ ] Liveness/readiness probes pointed at `/actuator/health/{liveness,readiness}`.
- [ ] `security_headers`, `cors`, and (for browser flows) `csrf` on.
- [ ] `request_log` + `request_metrics` on; logs shipped as JSON, metrics scraped
      from `/actuator/prometheus`.
- [ ] Correlation propagation verified end-to-end across services.
- [ ] JWT signing key injected from the environment / a secret store, not inline.
- [ ] In-memory event store + broker swapped for Postgres + Kafka at `build_app`.
- [ ] Graceful-shutdown drain budget tuned for the slowest in-flight transfer.
- [ ] The verification gate green: `cargo test -p firefly-sample-lumen` and
      `--features streaming`, plus `clippy -D warnings` and `fmt --check`.

## What changed in Lumen

This chapter wired the entry point and the optional stream — the last code the
arc adds:

- **`src/main.rs`** boots the whole service: `build_app` → `init_logging` →
  assemble the API router and the actuator router (with the info contributor) →
  start the scheduler on a background task → `print_banner` → run the lifecycle
  `Application` with two `on_server` listeners and graceful SIGINT/SIGTERM drain.
  A clean shutdown is a *cancelled* error and exits zero.
- **`Cargo.toml`** declares the `streaming` feature; **`src/web.rs`** adds the
  feature-gated `streaming_router` / `stream_events` handler that returns a
  `Flux<WalletEvent>` as NDJSON (default) or SSE (`?format=sse`), with the 404
  resolved before the response head.
- **`tests/streaming.rs`** proves the NDJSON default, the SSE switch, and the
  404, all behind the `streaming` feature.
- The chapter showed the one-line **in-memory → Postgres + Kafka** swap at
  `build_app`, proving the port-and-adapter design end to end.

## Exercises

1. **Run and drain.** `cargo run --bin lumen`, open a wallet, then Ctrl-C and
   watch the graceful drain. Confirm the process exits zero.
2. **Override the ports.** Start Lumen with `LUMEN_ADDR=0.0.0.0:9000
   LUMEN_ADMIN_ADDR=0.0.0.0:9001 cargo run --bin lumen` and confirm the API and
   actuator move.
3. **Stream the history.** Build with `--features streaming`, open a wallet and
   deposit, then `curl http://127.0.0.1:8080/api/v1/wallets/<id>/events` (NDJSON)
   and `...?format=sse` (SSE). Compare the two content types.
4. **Sketch the Postgres swap.** Write the two lines in `build_app` that would
   replace `MemoryEventStore` with a Postgres-backed `EventStore`, and explain in
   one sentence why `Ledger`, the projection, and the tests need no change.

That completes the guided tour of Lumen. The remaining chapters revisit the
declarative macros as a capstone and provide reference material: a
[Spring Boot migration map](./90-appendix-spring.md), the
[Module Index](./91-appendix-modules.md), and the [Glossary](./92-glossary.md).
