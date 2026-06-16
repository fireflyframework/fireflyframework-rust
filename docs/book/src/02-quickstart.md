# Quickstart

This is where **Lumen** — the digital-wallet and ledger service you will grow
across the rest of the book — first comes to life. By the end of this chapter
Lumen exists as a real crate: it compiles, prints a banner, serves a live
management surface, and shuts down gracefully. It does almost nothing else yet,
and that is deliberate. Everything from here on is *additive* — every later
chapter slices a little more out of the finished
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen)
crate and folds it back into the story, and nothing you write now gets thrown
away.

We will take two passes at the same goal. First we will scaffold the crate with
the `firefly` CLI (the fast path), then we will build the identical crate by
hand so that every line is something you typed and understand. Both land on the
same single-binary shape that the rest of the book assumes.

By the end of this chapter you will:

- Scaffold a Firefly project two ways — with the `firefly new` CLI and from a
  bare `cargo new`.
- Understand why a Firefly service depends on a *single* crate, the `firefly`
  facade, instead of a constellation of starter artifacts.
- Write the one-line `main` that boots and serves the whole service, and explain
  what each stage of `run()` does.
- Run Lumen and reach its two ports — the public API on `8080` and the
  management surface (actuator, admin dashboard, API docs) on `8081`.
- Read the startup report and confirm Lumen's health and build metadata with
  `curl`.

## Concepts you will meet

Before the first command, here are the three ideas this chapter leans on. Each
is reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — facade crate.** A *facade* is a single crate that
> re-exports a whole family of crates (and their macros) so that you depend on
> one name instead of many. Firefly ships its entire framework behind the
> `firefly` facade. The Spring analog is a Spring Boot *starter* — except here
> there is exactly one, and it covers everything.

> **Note** **Key term — bean.** A *bean* is an object the framework constructs
> and manages for you, then hands to whoever needs it. You declare beans; the
> framework discovers them at startup and wires them together. This is exactly
> Spring's notion of a bean managed by the application context.

> **Note** **Key term — actuator / management surface.** The *management
> surface* is a set of operational HTTP endpoints — health checks, build info,
> metrics, configuration introspection — that exist for operators and tooling,
> not for end users. Firefly serves them on a separate port from your business
> API. This mirrors Spring Boot Actuator.

## Step 1 — Check your toolchain

You need a recent stable Rust toolchain and nothing else. Lumen's default stack
requires **no external infrastructure** — its event store, event broker, and
read model are all pure Rust running in-process.

```bash
rustc --version   # 1.88 or later
cargo --version
```

> **Tip** **Checkpoint.** Both commands print a version. If `rustc` reports
> anything below 1.88, update with `rustup update stable` before continuing.

You will swap the in-process pieces for real infrastructure (Postgres, Kafka) in
[Production & Deployment](./20-production.md), but never before you are ready —
the whole book runs against the in-process defaults.

## Step 2 — Scaffold with the `firefly` CLI (Path A)

The fastest way to a running service is the developer CLI. Install it once, then
ask it to generate the project.

> **Note** **Key term — archetype.** An *archetype* is a project template that
> decides the starting shape of your crate — which modules exist, which Firefly
> features are switched on, and what the example code looks like. The CLI ships
> several (`core`, `web-api`, `web`, `hexagonal`, `library`, `cli`). The Spring
> analog is a Spring Initializr "project type" plus its preselected
> dependencies.

```bash
cargo install --path crates/cli      # from a checkout of the framework
# or, once published: cargo install firefly-cli

firefly new lumen --archetype web-api --features web,cqrs --git
cd lumen
cargo run
```

What just happened: `firefly new` wrote a Cargo crate with a `src/` tree, a
`firefly.yaml`, a `.gitignore`, a `README.md`, a `Dockerfile`, and a `tests/`
directory, then (because of `--git`) initialized a Git repository with a first
commit. The `web-api` archetype is the right starting shape for Lumen — a web
service with the CQRS bus already wired — and `--features web,cqrs` switches on
exactly those two subsystems. `cargo run` compiles and boots the service.

> **Note** **Key term — CQRS.** *Command/Query Responsibility Segregation* is a
> pattern that routes state-changing **commands** and read-only **queries**
> through separate handlers on a shared *bus*. You will build Lumen's command
> and query handlers in later chapters; for now it is enough that the `cqrs`
> feature reserves the wiring.

> **Tip** Run `firefly new --list` to print every archetype and feature flag, or
> `firefly new lumen --dry-run` to preview the exact file plan without writing a
> single file. See [The CLI](./19-cli.md) for the full generator catalogue.

> **Tip** **Checkpoint.** After `cargo run` you should see the Firefly banner
> followed by a `::`-prefixed startup report and two URLs (the admin dashboard
> and the API docs). If you got that far, skip to [Step 7](#step-7--run-it). If
> you want to understand every generated line, do Steps 3–6 by hand instead.

## Step 3 — Build the crate by hand (Path B)

The CLI is convenient, but the rest of the book lines up with `samples/lumen`
listing for listing, and the surest way to follow along is to type the crate
yourself. Start from a bare Cargo binary.

```bash
cargo new lumen
cd lumen
```

What just happened: `cargo new` created a binary crate — a `Cargo.toml` and a
placeholder `src/main.rs`. Over the next three steps you will replace both with
Lumen's real contents.

> **Tip** **Checkpoint.** `ls` shows a `Cargo.toml` and a `src/` directory.
> `cargo run` prints `Hello, world!`. That placeholder is the last code in this
> book that Firefly does *not* manage for you.

## Step 4 — Depend on the one crate that is the framework

Open `Cargo.toml`. This is where the one-dependency story becomes concrete. The
whole framework — CQRS, dependency injection, the reactive web stack, event
sourcing, saga orchestration, scheduling, security, observability — and *every*
`#[derive(...)]` / `#[...]` macro arrive through a single crate.

```toml
# Cargo.toml
[dependencies]
# The one-dependency front door: the `firefly` facade re-exports the whole
# framework AND every macro. Generated code resolves runtime types through the
# facade, so Lumen never lists the underlying `firefly-*` crates. The `admin`
# feature pulls in the self-hosted admin dashboard the management port mounts.
firefly = { version = "26.6.28", features = ["admin"] }
```

What just happened: that one line is the entire framework. Every later chapter
adds *code*, not dependencies — you will not edit this `firefly` line again.

> **Design note.** Many frameworks make you assemble a constellation of starter
> or plugin artifacts and keep their versions aligned by hand. Firefly collapses
> all of that into one `firefly` line: there is no starter to forget and no
> version skew between subsystems like `firefly-web` and `firefly-cqrs` — every
> `firefly-*` crate ships as one calendar-versioned release (here `26.6.28`),
> and you depend on the facade.

A Firefly service still writes directly against a few ecosystem crates: `axum`
(you author the controller handlers), `serde` / `serde_json` (your messages and
event payloads are serializable), the async runtime, and the id/clock crates the
domain uses. Add them, plus the feature flag that gates the streaming endpoint:

```toml
# The ecosystem crates a Firefly service still uses directly.
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# The async runtime for `#[tokio::main]`, and the id/clock crates the domain
# uses for wallet ids and event timestamps.
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
async-trait = "0.1"

[features]
# The reactive streaming endpoint is feature-gated so the teaching baseline
# stays lean; the production chapter turns it on. It needs nothing beyond the
# `firefly` facade.
default = []
streaming = []
```

What just happened: you declared the handful of crates you will write code
against directly, and a `streaming` feature flag that stays off by default.
Everything else flows in through `firefly`.

> **Tip** **Checkpoint.** Run `cargo build`. It downloads and compiles the
> framework (the first build is the slow one). A clean compile here means the
> facade and your direct dependencies all resolve.

## Step 5 — Write the one-line `main`

A Firefly service has exactly one entry point: `main`. There is **no composition
root, no `build_app`, and no application struct** to assemble by hand. Lumen is a
single-binary crate, so `src/main.rs` is the crate root — a few `mod`
declarations and a `main` that hands the whole service to the framework.

> **Note** **Key term — composition root.** The *composition root* is the one
> place in a program where the object graph is assembled — where every component
> is constructed and connected. In many frameworks you write this by hand. In
> Firefly the framework *is* the composition root: it scans your beans and wires
> them, so you never spell out the graph in a function.

Replace the contents of `src/main.rs` with the module list and entry point:

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

What just happened, line by line:

- The `mod` declarations name the modules Lumen will grow into. They are listed
  now so `main.rs` never changes again; you will fill each in across the book.
  Until a module file exists this list will not compile, so when you follow along
  for real you add the `mod` line in the same chapter that adds the module. For
  this quickstart, the only one you need is whatever you choose to keep — the
  point is the shape of `main`.
- `#[tokio::main]` turns `async fn main` into a normal `main` backed by the Tokio
  runtime, which Firefly needs because the whole stack is asynchronous.
- `Result<(), firefly::BoxError>` is the return type. `BoxError` is Firefly's
  boxed error type (`Box<dyn std::error::Error + Send + Sync>`); returning it lets
  you use `?` on the bootstrap and lets a startup failure surface as a non-zero
  exit.
- `firefly::FireflyApplication::new("lumen").run().await` is the whole service.
  `new("lumen")` names the application (the name shows up in the banner and in
  `/actuator/info`); `.run().await` boots and serves it.

> **Design note.** `FireflyApplication::new(name).run()` is the Rust analog of
> Spring Boot's `SpringApplication.run(App.class, args)`. That single call *is*
> the composition root — the framework assembles the object graph from the beans
> it scans rather than you spelling it out in a function. Nothing is reflective
> or hidden: the startup report (Step 7) logs exactly what was wired, so "what is
> running" is printed line-by-line at boot.

If you want to follow along with the smallest thing that compiles, drop the `mod`
lines and keep just the `main` function and the `#![allow(dead_code)]` attribute.
The full module list above is the real Lumen shape the rest of the book assumes.

> **Note** **Key term — application name and version.** Lumen keeps its name and
> version in two constants next to its HTTP surface, in `src/web.rs`. The version
> is sourced from the framework itself, so it tracks the release you depend on:
>
> ```rust,ignore
> // src/web.rs
> /// Lumen's application name (banner + `/actuator/info`).
> pub const APP_NAME: &str = "lumen";
>
> /// The released framework version, surfaced in the banner.
> pub const VERSION: &str = firefly::VERSION;
> ```

## Step 6 — Understand what `run()` does

`run()` is one line in your code and an entire boot pipeline underneath — the
work a service used to hand-roll in a composition root. Knowing the stages pays
off in every later chapter, because each chapter adds a bean that one of these
stages discovers. In order, `run()`:

- **Builds the web stack** — the RFC 9457 problem renderer, correlation-id
  propagation, idempotency replay, the in-process cache, the CQRS bus, the event
  broker, the health and metrics registries, the scheduler, and the web
  batteries (CORS, security headers, request metrics, the access log).
- **Component-scans the DI container** — it auto-registers the framework's
  infrastructure beans, then discovers and wires every app bean you declared:
  `#[derive(Configuration)]` + `#[bean]` factories, `#[derive(Controller)]`
  controllers, and `#[autowired]` fields. Any `async fn` bean factory (a DB pool,
  a broker dial) is awaited here so async beans are live before anything resolves
  them — and a construction error aborts startup (fail-fast).
- **Auto-configures the CQRS bus** — correlation propagation always; the
  read-cache middleware whenever a `QueryCache` bean is present.
- **Auto-discovers security** — the `FilterChain` and `BearerLayer` DI beans
  (Spring's `SecurityFilterChain`), layered onto the API with no `.security(...)`
  call needed.
- **Auto-mounts every controller** — each `#[rest_controller]` is mounted from
  the container with its state resolved automatically, and every
  `RouteContributor` bean's routes are merged in.
- **Drains the discovered handlers** — the inventory-registered CQRS command and
  query handlers, EDA event listeners, and `#[scheduled]` tasks, including the
  ones declared as bean methods that autowire their collaborators.
- **Builds the OpenAPI docs** from the live inventory and self-hosts the admin
  dashboard, both on the management port, wired to the real components.
- **Prints a Spring-style startup report** — the active profiles, every
  discovered bean, the mounted route table, and the handler/listener/scheduled
  counts — then **serves the public + management ports with graceful shutdown**.

A few properties recur in every chapter, so notice them now:

- **No `main` churn.** As Lumen grows a controller, a CQRS bus, an event-sourced
  ledger, and a security chain, `main` never changes — the new beans are
  *discovered*, not threaded through an entry point.
- **Two ports.** The public API serves on `8080`; the management surface
  (`/actuator/*` plus the self-hosted `/admin` dashboard plus the API docs) on
  `8081` by default — so operational endpoints never leak onto the public
  network.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 312" role="img"
     aria-label="Dual-port topology: the public API on port 8080 serves controllers, security and the RFC 9457 404 fallback; the management surface on port 8081 serves the actuator, the admin dashboard and the OpenAPI docs"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="24.0" y="18.5" width="248.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="16.0" width="248.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="148.0" y="33.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Public API  :8080</text><text x="148.0" y="47.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">client-facing</text>
<rect x="288.0" y="18.5" width="248.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="16.0" width="248.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="412.0" y="33.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Management  :8081</text><text x="412.0" y="47.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">operator-facing</text>
<rect x="24.0" y="80.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="78.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="148.0" y="101.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">#[rest_controller]</text><text x="148.0" y="115.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">your routes</text><rect x="24.0" y="150.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="148.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="148.0" y="171.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Security</text><text x="148.0" y="185.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">JWT · roles · sessions</text><rect x="24.0" y="220.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="218.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="148.0" y="241.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">RFC 9457 404</text><text x="148.0" y="255.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">problem+json fallback</text>
<rect x="288.0" y="80.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="78.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="412.0" y="101.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/actuator/*</text><text x="412.0" y="115.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">health · info · metrics</text><rect x="288.0" y="150.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="148.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="412.0" y="171.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/admin</text><text x="412.0" y="185.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">self-hosted dashboard</text><rect x="288.0" y="220.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="218.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="412.0" y="241.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/swagger-ui · /redoc</text><text x="412.0" y="255.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/v3/api-docs</text>
<text x="280.0" y="300.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">FIREFLY_SERVER_ADDR  ·  FIREFLY_MANAGEMENT_ADDR  override the binds</text>
</svg>
<figcaption>Two listeners, one process. The <strong>public API</strong> (<code>:8080</code>) serves your controllers, security and the RFC&nbsp;9457 <code>404</code> fallback; the <strong>management surface</strong> (<code>:8081</code>) serves the actuator, the self-hosted <code>/admin</code> dashboard and the OpenAPI docs — so operational endpoints never leak onto the public network.</figcaption>
</figure>
- **`FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`** override the bind
  addresses from the environment (defaulting to `0.0.0.0:8080` /
  `0.0.0.0:8081`). That is your first taste of the typed configuration story in
  [Configuration](./03-configuration.md).
- **Graceful shutdown is built in.** `run()` traps SIGINT/SIGTERM and drains
  in-flight requests before exiting; a cancelled run is a clean shutdown, not an
  error.

> **Note** **Testing seam.** `bootstrap()` is the sibling of `run()`: it
> assembles the same app but returns a `Bootstrapped` value *without serving*, so
> tests can drive the fully wired public router (`Bootstrapped::api_router`)
> in-process with no socket bound. You will lean on that hard in
> [Your First HTTP API](./06-first-http-api.md) and [Testing](./18-testing.md).

## Step 7 — Run it

```bash
cargo run
```

You will see the Firefly banner (ASCII art plus the framework version, your app
name, and the active profile), then the line-by-line startup report, followed by
the admin and API-docs URLs:

```text
:: admin dashboard :: http://0.0.0.0:8081/admin/
:: api docs (management) :: swagger-ui http://0.0.0.0:8081/swagger-ui | redoc http://0.0.0.0:8081/redoc | spec http://0.0.0.0:8081/v3/api-docs
:: active profiles :: default
:: beans (…) ::
:: routes (…) ::
:: cqrs handlers: … | event listeners: … | scheduled tasks: … | controllers: … ::
:: openapi :: … operations | … component schemas (served at /v3/api-docs) ::
```

What just happened: the framework booted the whole pipeline from Step 6 and is
now serving both ports. The `:: beans ::`, `:: routes ::`, and counts lines are
the inventory the framework wired — right now they are small because Lumen has no
business logic yet, and they grow as you add chapters.

> **Tip** **Checkpoint.** The process stays running and the last lines show the
> two URLs above. Open `http://localhost:8081/admin/` in a browser to see the
> self-hosted dashboard. Leave `cargo run` running in this terminal and use a
> second terminal for the `curl` checks below.

## Step 8 — Confirm health and build metadata

Even with no business routes of your own, the actuator is live on the management
port. From a second terminal:

```bash
# Liveness / readiness — on the management port, never the public one.
curl localhost:8081/actuator/health
# {"status":"UP", ...}
```

What just happened: `/actuator/health` aggregates every health indicator the
framework registered and reports the overall `status`. With the in-process
defaults everything is `"UP"`.

```bash
# Build metadata — the app name and version flow straight from
# `FireflyApplication::new("lumen")` and the framework version.
curl localhost:8081/actuator/info
# {"app":{"name":"lumen","version":"26.6.28"},"runtime":{...},"build":{...}}
```

What just happened: `/actuator/info` echoes the application name you passed to
`new(...)` and the version, alongside runtime and build details. Change the name
in `main` and this endpoint follows on the next run.

> **Tip** **Checkpoint.** Both `curl`s return JSON: health reports
> `"status":"UP"` and info reports `"app":{"name":"lumen", ...}`. If `curl` can
> connect but to neither path, confirm you are hitting `8081` (management), not
> `8080` (public). The public port has no `/actuator/*`.

## What you got for free

Without writing any of it yourself, Lumen already has:

- **RFC 9457 problem responses.** Any handler error renders as
  `application/problem+json`, an unmatched route returns a proper 404 problem
  document (not a blank body), and a panic is caught and rendered as a 500
  problem. You will use this from the very first endpoint in chapter 6.
- **Correlation IDs.** Every response echoes an `X-Correlation-Id`; an incoming
  one is honored and scoped through the whole request.
- **Idempotency.** Every `POST`/`PUT`/`PATCH` carrying an `Idempotency-Key`
  header is recorded; repeating the request replays the stored response, and
  reusing the key with a different body is a `409`.
- **A management surface.** `/actuator/{health,info,metrics,env,beans,mappings,
  conditions,...}` (the `beans` / `mappings` / `conditions` reports mirror Spring
  Boot Actuator's DI introspection) plus a self-hosted `/admin` dashboard, on a
  separate listener.
- **Auto-generated API docs.** Swagger UI (`/swagger-ui`), ReDoc (`/redoc`), and
  the OpenAPI 3.1 spec (`/v3/api-docs`) are served automatically on the
  **management** port (beside actuator and admin, not the public API) — zero app
  code.
- **Graceful shutdown.** `run()` traps SIGINT/SIGTERM and drains in-flight
  requests.

> **Design note.** Health, info, and metrics on a dedicated management port, a
> self-hosted admin dashboard, auto-generated API docs, and production-grade
> request middleware — all stood up by a single `FireflyApplication::new(...).run()`,
> with no config file to author first and no annotations to remember. This is
> Firefly's actuator surface, on by default.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| empty directory | a compiling crate whose only Firefly dependency is the `firefly` facade |
| no entry point | a one-line `main` over `FireflyApplication::new("lumen").run()` |
| nothing to run | a live actuator + admin on `:8081`, a public API on `:8080`, auto-generated docs, graceful shutdown |
| — | `APP_NAME` / `VERSION` constants that name the service and feed `/actuator/info` |

You also now know:

- Why a Firefly service depends on one crate — the `firefly` facade — instead of
  many starters, and how that avoids version skew.
- That `run()` is a full boot pipeline: build the web stack, component-scan the
  DI container, auto-configure CQRS, auto-discover security, auto-mount
  controllers, drain handlers, self-host admin and docs, then serve two ports.
- That `bootstrap()` is the test seam that returns the wired app without serving.

Lumen is now a real, runnable service that happens to have no business logic.
Every subsequent chapter fills that emptiness in — never by rewriting `main`,
only by declaring more beans for the framework to discover.

## Exercises

1. **Move the ports.** Start Lumen with `FIREFLY_SERVER_ADDR=127.0.0.1:9090
   FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091 cargo run`, then
   `curl localhost:9091/actuator/health`. Confirm the public and management
   surfaces moved independently — this is the seam
   [Configuration](./03-configuration.md) builds on.
2. **Read your own metadata.** `curl localhost:8081/actuator/info` and find the
   `app.name` / `app.version` values. Change the name passed to
   `FireflyApplication::new(...)`, re-run, and watch the banner and
   `/actuator/info` both follow.
3. **Read the startup report.** Run Lumen and read the line-by-line boot log: the
   active profiles, the discovered beans, the auto-mounted routes, and the
   handler/listener/scheduled counts. This is the inventory the framework wired —
   note how short it is today, then revisit it after a later chapter.
4. **Provoke graceful shutdown.** Run Lumen, then press `Ctrl-C`. Notice the
   process exits cleanly with no stack trace: `run()` treated the signal as a
   shutdown, not a failure.
5. **Preview the scaffold.** Even if you took Path B, run `firefly new lumen2
   --archetype web-api --features web,cqrs --dry-run` and compare the generated
   plan to the `Cargo.toml` and `main.rs` you wrote by hand.

## Where to go next

- Add typed, layered, profile-aware configuration in
  **[Configuration](./03-configuration.md)** — and replace those raw
  `FIREFLY_*` environment overrides with real properties.
- Learn how the framework wires the object graph it scans in
  **[Dependency Wiring](./04-dependency-wiring.md)**.
- Give Lumen its first real endpoints in
  **[Your First HTTP API](./06-first-http-api.md)**.
