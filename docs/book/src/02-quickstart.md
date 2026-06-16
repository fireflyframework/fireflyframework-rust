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
rustc --version   # 1.88 or later
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
firefly = "26.6.7"

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

> **Design note.** Many frameworks make you assemble a constellation of
> starter or plugin artifacts and keep their versions aligned by hand. Firefly
> collapses all of that into one `firefly` line: there is no starter to forget
> and no version skew between subsystems like `firefly-web` and `firefly-cqrs` —
> every `firefly-*` crate ships as one calendar-versioned release, and you
> depend on the facade.

## A one-line `main`

A Firefly service has one entry point: `main`. There is **no composition root,
no `build_app`, and no application struct** to assemble by hand. Lumen is a
single-binary crate — `src/main.rs` is the crate root: a few `mod` declarations
and a `main` that hands the whole service to the framework in one line:

```rust,ignore
// src/main.rs
#![allow(dead_code)]

mod commands;
mod compliance;
mod domain;
mod housekeeping;
mod ledger;
mod money;
mod security;
mod tcc_transfer;
mod transfer;
mod web;

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

`FireflyApplication::new("lumen").run()` is the Rust analog of Spring Boot's
`SpringApplication.run(App.class, args)`. That single call boots and serves the
whole service. Everything else in the crate is *declarative app code* the
framework discovers — there is nothing to wire by hand. (The two service
constants live next to the HTTP surface in `src/web.rs`:)

```rust,ignore
// src/web.rs
/// Lumen's application name (banner + `/actuator/info`).
pub const APP_NAME: &str = "lumen";

/// The released framework version, surfaced in the banner.
pub const VERSION: &str = firefly::VERSION;
```

### What `run()` does, step by step

When `run()` is called, `FireflyApplication` performs the entire boot pipeline a
service used to hand-roll in a composition root:

- **Builds the web stack** — the RFC 9457 problem renderer, correlation-id
  propagation, idempotency replay, the in-process cache, the CQRS bus, the event
  broker, the health and metrics registries, the scheduler, plus the web
  batteries (CORS, security headers, request metrics, the access log).
- **Component-scans the DI container** — it auto-registers the framework's
  infrastructure beans, then discovers and wires every app bean: your
  `#[derive(Configuration)]` + `#[bean]` factories, `#[derive(Controller)]`
  controllers, and `#[autowired]` fields.
- **Auto-configures the CQRS bus** — correlation propagation always; the
  read-cache middleware whenever a `QueryCache` bean is present.
- **Auto-discovers security** — the `FilterChain` and `BearerLayer` DI beans
  (Spring's `SecurityFilterChain`), layered onto the API with no `.security(...)`
  call.
- **Auto-mounts every controller** — each `#[rest_controller]` is mounted from
  the container (state resolved automatically), and every `RouteContributor`
  bean's routes are merged in.
- **Drains the discovered handlers** — the inventory-registered CQRS command /
  query handlers, EDA event listeners, and `#[scheduled]` tasks.
- **Self-hosts the admin dashboard** on the management port, wired to the live
  components with real env / config / mappings data, and auto-serves the
  generated OpenAPI docs (Swagger UI + ReDoc).
- **Prints a pyfly/Spring-style startup report** — a line-by-line log of the
  active profiles, every discovered bean, the mounted route table, and the
  handler/listener/scheduled counts — then **serves the public + management
  ports with graceful shutdown**.

A few things to notice, because they recur in every chapter:

- **No `main` churn.** As Lumen grows a controller, a CQRS bus, an event-sourced
  ledger, and a security chain, `main` never changes — the new beans are
  *discovered*, not threaded through an entry point.
- **Two ports.** The public API serves on `8080` and the management surface
  (`/actuator/*` + the self-hosted `/admin` dashboard) on `8081` by default, so
  management never leaks onto the public network.
- **`FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`** override the bind
  addresses from the environment (defaulting to `0.0.0.0:8080` / `0.0.0.0:8081`)
  — your first taste of the typed configuration story in
  [Configuration](./03-configuration.md).
- **Graceful shutdown is built in.** `run()` traps SIGINT/SIGTERM and drains
  in-flight requests before exiting; a cancelled run is a clean shutdown, not an
  error.

> **Design note.** `FireflyApplication::new(name).run()` *is* the composition
> root — the framework assembles the object graph from the beans it scans,
> rather than you spelling it out in a function. Nothing is reflective or
> hidden: the startup report logs exactly what was wired (every bean, every
> route, every handler), so "what is running" is printed line-by-line at boot.

> **Testing seam.** `bootstrap()` is the sibling of `run()`: it assembles the
> same app but returns it *without serving*, so the tests can drive the fully
> wired public router in-process with no socket bound. You will lean on that
> hard in [Your First HTTP API](./06-first-http-api.md) and
> [Testing](./18-testing.md).

## Run it

```bash
cargo run
```

You will see the Firefly banner, then the line-by-line startup report — the
active profiles, the discovered beans, the auto-mounted routes, and the
handler/listener/scheduled counts — followed by the admin and API-docs URLs:

```text
:: admin dashboard :: http://0.0.0.0:8081/admin/
:: api docs :: swagger-ui http://0.0.0.0:8080/swagger-ui | redoc http://0.0.0.0:8080/redoc | spec http://0.0.0.0:8080/v3/api-docs
:: active profiles :: default
:: beans (…) ::
:: routes (…) ::
```

Even with no business routes of your own yet, the actuator is live on the
management port:

```bash
# Liveness / readiness — on the management port, never the public one.
curl localhost:8081/actuator/health
# {"status":"UP", ...}

# Build metadata — the app name and version flow straight from
# `FireflyApplication::new(...).version(...)`.
curl localhost:8081/actuator/info
# {"app":{"name":"lumen","version":"26.6.7"}, ...}
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
- **A management surface.** `/actuator/{health,info,metrics,env,beans,mappings,
  conditions,...}` (the `beans` / `mappings` / `conditions` DI-introspection
  reports mirror Spring Boot Actuator's) plus a self-hosted `/admin` dashboard,
  on a separate listener.
- **Auto-generated API docs.** Swagger UI (`/swagger-ui`), ReDoc (`/redoc`), and
  the OpenAPI 3.1 spec (`/v3/api-docs`) are served automatically — zero app code.
- **Graceful shutdown.** `run()` traps SIGINT/SIGTERM and drains.

> **Design note.** Health, info, and metrics on a dedicated management port, a
> self-hosted admin dashboard, auto-generated API docs, and production-grade
> request middleware — all stood up by a single `FireflyApplication::new(...).run()`
> with no config file to author first and no annotations to remember. This is
> Firefly's actuator surface, on by default.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| empty directory | a compiling `firefly-sample-lumen` crate with one Firefly dependency |
| no entry point | a one-line `main` over `FireflyApplication::new("lumen").run()` |
| nothing to run | a live actuator + admin on `:8081`, a public API on `:8080`, auto-generated docs, graceful shutdown |
| — | `APP_NAME` / `VERSION` constants that name the service and feed `/actuator/info` |

Lumen is now a real, runnable service that happens to have no business logic.
Every subsequent chapter fills that emptiness in — never by rewriting `main`,
only by declaring more beans for the framework to discover.

## Exercises

1. **Move the ports.** Start Lumen with `FIREFLY_SERVER_ADDR=127.0.0.1:9090
   FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091 cargo run`, then `curl
   localhost:9091/actuator/health`. Confirm the public and management surfaces
   moved independently — this is the seam [Configuration](./03-configuration.md)
   builds on.
2. **Read your own metadata.** `curl localhost:8081/actuator/info` and find the
   `app.name` / `app.version` values. Change the name passed to
   `FireflyApplication::new(...)`, re-run, and watch the banner and
   `/actuator/info` both follow.
3. **Read the startup report.** Run Lumen and read the line-by-line boot log:
   the active profiles, the discovered beans, the auto-mounted routes, and the
   handler/listener/scheduled counts. This is the inventory the framework wired.
4. **Provoke graceful shutdown.** Run Lumen, then press `Ctrl-C`. Notice the
   process exits cleanly (no stack trace): `run()` treated the signal as a
   shutdown, not a failure.
5. **Preview the scaffold.** Even if you took Path B, run `firefly new lumen2
   --archetype web-api --dry-run` and compare the generated plan to the files
   you wrote by hand.

## Where to go next

- Add typed, layered, profile-aware configuration in
  **[Configuration](./03-configuration.md)** — and replace those raw
  `std::env::var` calls.
- Learn how the framework wires the object graph it scans in
  **[Dependency Wiring](./04-dependency-wiring.md)**.
- Give Lumen its first real endpoints in
  **[Your First HTTP API](./06-first-http-api.md)**.
