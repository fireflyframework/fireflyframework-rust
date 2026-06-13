# Quickstart

This chapter gets you from an empty directory to a running service with an
idempotent REST endpoint, a streaming reactive endpoint, and a live actuator —
in a few minutes. Two paths: the `firefly` CLI (fastest) or plain `cargo`.

## Prerequisites

```bash
rustc --version   # 1.85 or later (edition 2021)
cargo --version
```

That is all you need. The default stack requires **no external infrastructure** —
the in-process cache and event broker are pure Rust.

## Path A — scaffold with the `firefly` CLI

Install the developer CLI once, then scaffold a web-api project:

```bash
cargo install --path crates/cli      # from a checkout of the framework
# or: cargo install firefly-cli

firefly new orders --archetype web-api --features web,cqrs --git
cd orders
cargo run
```

`firefly new` generates a workspace-less Cargo crate with a `src/` tree, a
`firefly.yaml`, a `.gitignore`, a `README.md`, a `Dockerfile`, and a `tests/`
directory. See [The CLI](./19-cli.md) for every archetype and generator.

> **Tip** — Run `firefly new --list` to see every archetype (`core`, `web-api`,
> `web`, `hexagonal`, `library`, `cli`) and feature flag, or
> `firefly new svc --dry-run` to preview the plan without writing files.

## Path B — start from cargo

Create a binary crate and add the starter plus axum and tokio:

```bash
cargo new orders
cd orders
```

```toml
# Cargo.toml
[dependencies]
firefly-starter-core = "26.6.3"
firefly-reactive = "26.6.3"
firefly-web = "26.6.3"
axum = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
serde_json = "1"
```

> Prefer the one-dependency front door? Replace the three `firefly-*` lines with
> a single `firefly = "26.6.3"` and `use firefly::prelude::*;`. See the
> [Declarative Services with Macros](./21-declarative-macros.md) chapter.

## A running service in one file

Replace `src/main.rs` with the following. One `Core::new` wires the problem
renderer, correlation propagation, idempotency replay, cache, CQRS bus, event
broker, health, metrics, and scheduler. We mount a plain JSON route, a reactive
`Mono` route, and a streaming `Flux` (NDJSON) route — then serve the public API
and the actuator on separate ports with signal-aware graceful shutdown.

```rust,no_run
use axum::{routing::get, Router};
use firefly_reactive::{Flux, Mono};
use firefly_starter_core::{Core, CoreConfig};
use firefly_web::{MonoJson, NdJson};

// A plain JSON handler.
async fn list_orders() -> &'static str {
    "[]"
}

// A reactive handler: resolve a Mono to 200 application/json
// (Ok(None) -> 404 problem+json, Err -> that error's RFC 7807 response).
async fn one_order() -> MonoJson<serde_json::Value> {
    MonoJson(Mono::just(serde_json::json!({ "id": "o1", "customer": "alice" })))
}

// A streaming handler: each Flux element becomes one application/x-ndjson
// line, flushed incrementally with real backpressure.
async fn stream_orders() -> NdJson<i64> {
    NdJson(Flux::range(1, 3))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core = Core::new(CoreConfig {
        app_name: "orders".into(),
        app_version: "1.0.0".into(),
        ..CoreConfig::default()
    });
    core.init_logging()?;
    core.print_banner();

    let api = core.apply_middleware(
        Router::new()
            .route("/orders", get(list_orders))
            .route("/orders/one", get(one_order))
            .route("/orders/stream", get(stream_orders)),
    );
    let admin = core.actuator_router(Vec::new());

    let app = core
        .new_application()
        .on_server("api", move |shutdown| async move {
            let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
            axum::serve(listener, api)
                .with_graceful_shutdown(shutdown.wait())
                .await?;
            Ok(())
        })
        .on_server("admin", move |shutdown| async move {
            let listener = tokio::net::TcpListener::bind("0.0.0.0:8081").await?;
            axum::serve(listener, admin)
                .with_graceful_shutdown(shutdown.wait())
                .await?;
            Ok(())
        });

    app.run().await?; // blocks until ctrl-c / SIGTERM
    Ok(())
}
```

```bash
cargo run
```

## What you just got

Hit the endpoints:

```bash
# Plain JSON.
curl localhost:8080/orders
# []

# Reactive Mono -> 200 application/json.
curl localhost:8080/orders/one
# {"id":"o1","customer":"alice"}

# Reactive Flux -> streamed application/x-ndjson, one line per element.
curl -N localhost:8080/orders/stream
# 1
# 2
# 3

# Actuator health, info, metrics — on the admin port.
curl localhost:8081/actuator/health
# {"status":"UP", ...}
```

Without writing any of it yourself, the service already has:

- **RFC 7807 errors.** Any handler error renders as `application/problem+json`,
  and a panic is caught and rendered as a 500 problem.
- **Correlation IDs.** Every response echoes an `X-Correlation-Id`; an incoming
  one is honoured and scoped through the whole request.
- **Idempotency.** Every `POST`/`PUT`/`PATCH` carrying an `Idempotency-Key`
  header is recorded; repeating the request replays the stored response with
  `Idempotent-Replay: true`, and reusing the key with a different body is a 409.
- **A reactive surface.** `Mono`/`Flux` drop straight into axum handlers via the
  `MonoJson` / `NdJson` / `Sse` responders, with true streaming and backpressure.
- **A management surface.** `/actuator/{health,info,metrics,env,tasks,version}`
  on a separate listener so it never leaks onto the public network.
- **Graceful shutdown.** `app.run()` traps SIGINT/SIGTERM and drains in-flight
  requests before exiting.

## Where to go next

- The reactive responders you just used are explained in depth in
  **[The Reactive Model](./05-reactive-model.md)** — read it next.
- Add typed configuration in **[Configuration](./03-configuration.md)**.
- Build out real handlers and routes in
  **[Your First HTTP API](./06-first-http-api.md)** and **[CQRS](./09-cqrs.md)**.
