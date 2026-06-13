# Quickstart

> By the end of this chapter **Lumen** — the digital-wallet and ledger service
> you will grow across the rest of the book — exists as a real crate: it
> compiles, prints a banner, serves a live actuator, and shuts down gracefully.
> It does almost nothing yet. That is the point. Everything from here on is
> *additive*: every later chapter slices a little more out of the finished
> [`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen)
> crate and folds it back into the story, and nothing you write now gets thrown
> away.

This chapter takes you from an empty directory to a running Lumen process in a
few minutes. Two paths get you there: the `firefly` CLI (fastest) or plain
`cargo`. Either way, the destination is the same — a binary crate whose *only*
Firefly dependency is the [`firefly`](./21-declarative-macros.md) facade.

## Prerequisites

```bash
rustc --version   # 1.85 or later
cargo --version
```

That is all you need. Lumen's default stack requires **no external
infrastructure** — the event store, the event broker, and the read model are
all pure Rust, in-process. You will swap each of them for real infrastructure
(Postgres, Kafka) in [Production & Deployment](./20-production.md), but never
before you are ready.

## Path A — scaffold with the `firefly` CLI

Install the developer CLI once, then scaffold the project:

```bash
cargo install --path crates/cli      # from a checkout of the framework
# or: cargo install firefly-cli

firefly new lumen --archetype web-api --features web,cqrs --git
cd lumen
cargo run
```

`firefly new` generates a Cargo crate with a `src/` tree, a `firefly.yaml`, a
`.gitignore`, a `README.md`, a `Dockerfile`, and a `tests/` directory. The
`web-api` archetype is the right starting shape for Lumen: a web service with
the CQRS bus already wired. See [The CLI](./19-cli.md) for every archetype and
generator.

> **Tip** — Run `firefly new --list` to see every archetype (`core`, `web-api`,
> `web`, `hexagonal`, `library`, `cli`) and feature flag, or
> `firefly new lumen --dry-run` to preview the plan without writing files.

## Path B — start from cargo

If you would rather see every line yourself, create the crate by hand. This is
exactly the shape `samples/lumen` has, so the rest of the book lines up with it
listing for listing.

```bash
cargo new lumen
cd lumen
```

Lumen's `Cargo.toml` makes the one-dependency story concrete. The whole
framework — CQRS, dependency injection, the reactive web stack, event sourcing,
saga orchestration, scheduling, security, observability — and *every*
`#[derive(...)]` / `#[...]` macro arrive through a single crate:

```toml
# Cargo.toml
[dependencies]
# The one-dependency front door: the `firefly` facade re-exports the whole
# framework AND every macro. Generated code resolves runtime types through the
# facade, so Lumen never lists the underlying `firefly-*` crates.
firefly = "26.6.3"

# The two ecosystem crates a Firefly service still writes against directly:
# axum (you author the controller handlers) and serde (your messages and event
# payloads are Serialize/Deserialize). serde_json encodes the event payloads.
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# The async runtime, and the id/clock crates the domain uses later.
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
async-trait = "0.1"

[features]
# The reactive streaming endpoint is feature-gated so the teaching baseline
# stays lean; chapter 20 turns it on. It needs nothing beyond `firefly`.
default = []
streaming = []
```

> **Spring parity.** A Spring Boot service pulls in a constellation of
> `spring-boot-starter-*` artifacts, and pyfly enables subsystems with
> `@enable_domain_stack`. Firefly for Rust collapses all of that into one
> `firefly` line. There is no starter to forget and no version skew between
> `firefly-web` and `firefly-cqrs` — they ship as one calendar-versioned
> release and you depend on the facade.

## Lumen's two entry points

A Firefly service has a *composition root* (where the application is assembled)
and a *process entry point* (`main`, where it is run). Keeping them apart is
what lets the tests drive the fully-wired app in-process without binding a
socket — you will lean on that hard in [Testing](./18-testing.md).

Lumen names the assembled application `LumenApp` and builds it in
`src/web.rs::build_app`. For now the body is almost empty; later chapters give
it a controller, a CQRS bus, an event-sourced ledger, and a security chain.
What matters in this chapter is the shape and the two constants that name the
service:

```rust,ignore
// src/web.rs
use firefly::prelude::*;
use firefly::starter_web::WebStack;

/// Lumen's application name (banner + `/actuator/info`).
pub const APP_NAME: &str = "lumen";

/// The released framework version, surfaced in the banner.
pub const VERSION: &str = firefly::VERSION;

/// The fully-assembled Lumen application. Right now it carries only the
/// web-tier stack; the bus, ledger, and read model arrive in later chapters.
pub struct LumenApp {
    /// The web-tier starter (CORS / security-headers / correlation / metrics
    /// on by default), which `Deref`s to the infrastructure `Core`.
    pub web: WebStack,
}

/// Assembles a `LumenApp` over **in-memory** infrastructure — the default for
/// tests and a no-infra `cargo run`.
pub async fn build_app() -> LumenApp {
    let web = WebStack::new(firefly::starter_web::CoreConfig {
        app_name: APP_NAME.into(),
        app_version: VERSION.into(),
        ..Default::default()
    });
    LumenApp { web }
}
```

`WebStack::new` is the one call that wires the whole web tier: the RFC 9457
problem renderer, correlation-id propagation, idempotency replay, the in-process
cache, the CQRS bus, the event broker, the health and metrics registries, the
scheduler, plus the web batteries (CORS, security headers, request metrics, the
access log) — all from those defaulted `CoreConfig` fields. `WebStack` derefs to
the infrastructure `Core`, so `app.web.bus`, `app.web.broker`, and friends are
right there when later chapters need them.

> **Spring parity.** `WebStack::new(CoreConfig { .. })` is the Rust spelling of
> `@SpringBootApplication` + auto-configuration (and of pyfly's
> `@enable_web_stack`): one declaration stands up the entire managed context.
> The difference is that nothing is reflective or hidden — `CoreConfig` is a
> plain struct whose fields *are* the knobs, so "what is wired" is exactly "what
> the struct says."

The process entry point lives in `src/main.rs`. It builds the app, turns on
logging, prints the banner, serves the public API and the actuator on separate
ports, and runs under the lifecycle `Application` with graceful shutdown:

```rust,ignore
// src/main.rs
use firefly_sample_lumen::web::{build_app, APP_NAME, VERSION};

/// Default bind address of the public API server.
const DEFAULT_ADDR: &str = "127.0.0.1:8080";
/// Default bind address of the admin (actuator) server.
const DEFAULT_ADMIN_ADDR: &str = "127.0.0.1:8081";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = build_app().await;
    // Best-effort: a test harness may already own the global subscriber.
    let _ = app.web.init_logging();

    let api = app.router();
    let admin = app.web.actuator_router(Vec::new());

    app.web.print_banner();
    println!(":: {APP_NAME} :: digital-wallet & ledger (v{VERSION})");

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
    Ok(())
}
```

A few things to notice, because they recur in every chapter:

- **`app.router()`** is the public surface. In this chapter it is empty; in
  [Your First HTTP API](./06-first-http-api.md) it gains the wallet routes, and
  in [Security](./14-security.md) it gains the JWT layer. `main` never changes
  when that happens — the composition root absorbs the growth.
- **`app.web.actuator_router(...)`** is the management surface, served on a
  *separate* port (`8081` by default) so it never leaks onto the public network.
- **`LUMEN_ADDR` / `LUMEN_ADMIN_ADDR`** override the bind addresses from the
  environment — your first taste of the typed configuration story in
  [Configuration](./03-configuration.md).
- **`application.run()`** traps SIGINT/SIGTERM and drains in-flight requests
  before exiting. A cancelled run is a clean shutdown, not an error.

## Run it

```bash
cargo run
```

You will see the Firefly banner, then Lumen's own line:

```text
:: lumen :: digital-wallet & ledger (v26.6.3)
```

Even with no business routes yet, the actuator is live on the admin port:

```bash
# Liveness / readiness — on the admin port, never the public one.
curl localhost:8081/actuator/health
# {"status":"UP", ...}

# Build metadata — note app_name and app_version flow straight from CoreConfig.
curl localhost:8081/actuator/info
# {"app":{"name":"lumen","version":"26.6.3"}, ...}
```

## What you got for free

Without writing any of it yourself, Lumen already has:

- **RFC 9457 problem responses.** Any handler error renders as
  `application/problem+json`, and a panic is caught and rendered as a 500
  problem. (You will use this from the very first endpoint in chapter 6.)
- **Correlation IDs.** Every response echoes an `X-Correlation-Id`; an incoming
  one is honored and scoped through the whole request.
- **Idempotency.** Every `POST`/`PUT`/`PATCH` carrying an `Idempotency-Key`
  header is recorded; repeating the request replays the stored response, and
  reusing the key with a different body is a `409`.
- **A management surface.** `/actuator/{health,info,metrics,...}` on a separate
  listener.
- **Graceful shutdown.** `application.run()` traps SIGINT/SIGTERM and drains.

> **Spring parity.** This is the Spring Boot Actuator experience — health, info,
> metrics on a management port, plus production-grade request middleware — but
> stood up by a single `WebStack::new`, with no `application.properties` to
> author first and no annotations to remember.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| empty directory | a compiling `firefly-sample-lumen` crate with one Firefly dependency |
| no entry point | `build_app()` composition root + a `main` that runs under the lifecycle `Application` |
| nothing to run | a live actuator on `:8081`, a (still-empty) public API on `:8080`, graceful shutdown |
| — | `APP_NAME` / `VERSION` constants that name the service and feed `/actuator/info` |

Lumen is now a real, runnable service that happens to have no business logic.
Every subsequent chapter fills that emptiness in — never by rewriting what you
have, only by extending the composition root.

## Exercises

1. **Move the ports.** Start Lumen with `LUMEN_ADDR=127.0.0.1:9090
   LUMEN_ADMIN_ADDR=127.0.0.1:9091 cargo run`, then `curl
   localhost:9091/actuator/health`. Confirm the public and admin surfaces moved
   independently — this is the seam [Configuration](./03-configuration.md)
   builds on.
2. **Read your own metadata.** `curl localhost:8081/actuator/info` and find the
   `app.name` / `app.version` values. Change `APP_NAME` in `web.rs`, re-run, and
   watch the banner and `/actuator/info` both follow — one constant, two
   surfaces.
3. **Provoke graceful shutdown.** Run Lumen, then press `Ctrl-C`. Notice the
   process exits cleanly (no stack trace): `application.run()` treated the
   signal as a shutdown, not a failure.
4. **Preview the scaffold.** Even if you took Path B, run `firefly new lumen2
   --archetype web-api --dry-run` and compare the generated plan to the files
   you wrote by hand.

## Where to go next

- Add typed, layered, profile-aware configuration in
  **[Configuration](./03-configuration.md)** — and replace those raw
  `std::env::var` calls.
- Learn how the composition root resolves collaborators in
  **[Dependency Wiring](./04-dependency-wiring.md)**.
- Give Lumen its first real endpoints in
  **[Your First HTTP API](./06-first-http-api.md)**.
