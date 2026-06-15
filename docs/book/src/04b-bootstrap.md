# Bootstrapping with FireflyApplication

> By the end of this chapter you will understand the single line that boots
> Lumen: how `FireflyApplication::new("lumen").run()` builds the web stack,
> component-scans the container, auto-configures the CQRS bus, discovers
> security, auto-mounts every `#[rest_controller]`, drains the inventory-
> registered handlers / listeners / scheduled tasks, serves OpenAPI docs and the
> self-hosted admin dashboard, prints a Spring-Boot-style startup report, and
> serves the public + management ports with graceful shutdown — all with **no
> composition root and no bootstrap file**.

Lumen's `main` is one line:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

This is the Rust analog of Spring Boot's `SpringApplication.run(App.class, args)`
and pyfly's `FireflyApplication("lumen").run()`. There is no `build_app`, no
hand-written router, no `register_*`/`subscribe_*`/`schedule_*` call site — the
framework discovers every one of those from the declarations you wrote next to
the code (the beans in [chapter 4](./04a-dependency-injection.md), the
controllers in [chapter 6](./06-first-http-api.md), the handlers in
[chapter 9](./09-cqrs.md), the listeners in
[chapter 10](./10-eda-messaging.md), the scheduled tasks in
[chapter 16](./16-scheduling-notifications.md)). `main` only names the app and
hands control to the framework.

## `new` / `run` / `bootstrap`

`FireflyApplication::new(name)` constructs the application. `run().await` boots
and serves until the process receives `SIGINT`/`SIGTERM`. For tests, you do not
want to bind a socket — so `bootstrap().await` does everything `run` does
*except* serve, returning a `Bootstrapped` whose `api_router` you can drive
in-process. Lumen's HTTP tests use exactly that:

```rust,ignore
// src/web.rs — the testable in-process router, no socket bound
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

So the production path (`run`) and the test path (`bootstrap` →
`tower::ServiceExt::oneshot`) assemble the **identical** app — the tests in
[chapter 18](./18-testing.md) exercise the same wiring `main` serves.

## The boot pipeline, step by step

`bootstrap()` runs a fixed pipeline. Each step ties to something the framework
*discovers and wires* for you:

1. **Build the web stack.** `WebStack::new(config)` brings up axum, the CQRS
   `Bus`, the EDA `Broker`, the `Scheduler`, the metric registry, the health
   composite, and the default middleware (correlation id, request metrics,
   idempotency, CORS, security headers). Security is *not* applied here — it
   comes from a bean after the scan — so the stack is built plain and mutable.

2. **Initialise logging.** The structured-logging subscriber is installed. When
   the `admin` feature is on, logs are also teed into the admin dashboard's
   in-memory capture buffer so `/admin` can show a live log tail.

3. **Component-scan the container.** The framework first **auto-registers its
   own infrastructure beans** (`web.register_beans(&container)` — the `Bus`,
   `Broker`, `Scheduler`, …), then `container.scan()` discovers, registers, and
   autowires **your** beans — every `#[derive(Component/Service/Repository/
   Configuration/Controller)]` and every `#[bean]` factory linked into the
   binary. For Lumen that is the `LumenBeans` `#[derive(Configuration)]` and its
   `#[bean]` factories (the event store, read model, query cache, JWT service,
   `FilterChain`, `BearerLayer`, ledger) plus the `WalletApi` controller bean.
   This is the link-time DI of [chapter 4](./04a-dependency-injection.md).

4. **Auto-configure the CQRS bus.** Correlation propagation is always layered
   on. If a `QueryCache` bean is present in the container, its read-cache
   middleware is layered on too — so Lumen's `GetWallet` 30s cache
   ([chapter 17](./17-caching.md)) is wired with no app code, just by *declaring
   the `QueryCache` bean*. (Validation middleware is already installed by the
   core.)

5. **Run the optional readiness hook.** Most apps — Lumen included — need none;
   the beans and the pipeline cover the wiring. `on_ready` exists for the rare
   case that wants the live collaborators (container, bus, broker, scheduler)
   after the scan but before serving.

6. **Auto-discover security.** With no explicit `.security(...)` call, the
   framework resolves the `FilterChain` bean (path-based RBAC) and the
   `BearerLayer` bean (token extraction) from the container and applies them —
   Spring's `SecurityFilterChain` bean, discovered. Lumen declares both as
   `#[bean]`s in `LumenBeans`, so the secured routes from
   [chapter 14](./14-security.md) light up automatically. (If an
   `ExceptionHandlerRegistry` bean is present, it is installed as the outermost
   advice layer — the `@ControllerAdvice` analog.)

7. **Auto-mount the routes.** `mount_controllers(&container)` resolves every
   `#[rest_controller]` and builds its router from the controller's autowired
   state bean; `mount_route_contributors(&container)` merges every
   `RouteContributor` bean (this is how Lumen's feature-gated streaming endpoint
   is added — by declaring a bean, not by editing a composition root).
   Resolving the controllers here also constructs their collaborators, including
   the `ledger` `#[bean]` that **seeds the event-sourcing projection** on
   construction.

8. **Drain the inventory.** The framework drains the link-time registries the
   macros filled at compile time:
   `register_discovered_handlers(&bus)` installs every `#[command_handler]` /
   `#[query_handler]`, `subscribe_discovered_listeners(broker)` subscribes every
   `#[event_listener]`, and `register_discovered_scheduled(&scheduler)`
   schedules every `#[scheduled]` task. No `register(&bus)` /
   `subscribe(&broker)` call sites — the declarations *are* the registration.

9. **Apply the middleware chain.** The discovered middleware is applied over the
   mounted routes, the bearer-auth layer is added, and (with the `admin`
   feature) a W3C trace layer is added that **originates and echoes**
   `traceparent` so every request is correlatable across services.

10. **Serve OpenAPI docs.** The spec is built from the **live inventory** —
    every `#[rest_controller]` route plus every `#[derive(Schema)]` DTO — and
    served at `/v3/api-docs` (+ `/openapi.json`), with Swagger UI at
    `/swagger-ui` and ReDoc at `/redoc`, *outside* the security chain so the
    docs are reachable without a token. This is auto-wired with no app code;
    [the next-but-one chapter](./06a-openapi.md) covers it in full.

11. **Install the default 404.** An unmatched route gets a proper RFC 9457
    `application/problem+json` 404 instead of axum's bare empty body (see
    [below](#the-default-404)).

12. **Build the management router.** The actuator endpoints
    (`/actuator/health|info|metrics|loggers|mappings|beans|conditions|env`) are
    assembled, and — with the `admin` feature — the **self-hosted admin
    dashboard** is mounted at `/admin/`, wired to the live components (health,
    metrics, the bus, the scheduler, the container, the environment snapshot,
    the trace buffer, the log buffer). [Chapter 15](./15-observability.md)
    covers the admin surface.

`bootstrap` returns the assembled `Bootstrapped`; `run` then calls `serve`.

## Builder knobs

`FireflyApplication` is a builder. Every knob is optional; Lumen uses only
`new` (in `main`) and `version` (in `build_router`). The real methods:

| Method | What it does |
|--------|--------------|
| `new(name)` | Names the app (banner + `/actuator/info`). Defaults the binds from `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`. |
| `version(v)` | Sets the version (banner + `/actuator/info`). |
| `configure(\|cfg\| { … })` | Tunes the `CoreConfig` in place — CORS, security headers, idempotency, the knobs of [chapter 3](./03-configuration.md). |
| `security(chain, bearer)` | Installs a `FilterChain` + `BearerLayer` **explicitly**, instead of discovering them from beans. |
| `on_ready(\|ctx\| async { … })` | A readiness hook over the live `container` / `bus` / `broker` / `scheduler`, run after the scan, before serving. |
| `extra_routes(\|container\| router)` | Merges extra non-`#[rest_controller]` routes built from the scanned container. |
| `info_contributor(c)` | Adds an `/actuator/info` contributor. |
| `api_addr(addr)` | Overrides the public API bind address. |
| `management_addr(addr)` | Overrides the management (actuator + admin) bind address. |
| `bootstrap()` | Assembles the app **without serving** (tests). |
| `run()` | Bootstraps and serves. |

```rust,ignore
// the knobs are chainable; Lumen needs almost none of them
firefly::FireflyApplication::new("orders")
    .version("1.0.0")
    .configure(|cfg| { /* tune the CoreConfig: CORS, security headers, … */ })
    .management_addr("127.0.0.1:9091")
    .run()
    .await
```

> **Design note.** Lumen declares security as `#[bean]`s rather than calling
> `.security(...)`, declares its streaming endpoint as a `RouteContributor`
> bean rather than calling `.extra_routes(...)`, and seeds its projection inside
> the `ledger` `#[bean]` rather than in `.on_ready(...)`. The explicit builder
> knobs exist for apps that prefer a touch of imperative wiring; the *bean*
> route is the framework's preferred, fully-declarative path — declaration next
> to the code.

## Env-overridable binds

By default the public API binds `0.0.0.0:8080` and the management surface binds
`0.0.0.0:8081`. Override either without touching code:

```bash
FIREFLY_SERVER_ADDR=127.0.0.1:9090 \
FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091 \
cargo run --bin lumen
```

`new` reads `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` at construction;
`api_addr(...)` / `management_addr(...)` override them in code if you need to.

## The startup report

Just before serving, `serve` prints the banner, the docs URLs, and then
`log_startup_report(&container)` — the Spring-Boot/pyfly-style **line-by-line
report** so a boot log reads like Spring Boot's console: you can see exactly
what the framework wired. The format is:

```text
:: active profiles :: default
:: beans (N) ::
     [stereotype   ] name                   scope      (TypeName)
     …one line per scanned bean, sorted by stereotype then name…
:: routes (N) ::
     METHOD path                             -> Controller::handler
     …one line per auto-mounted route, sorted by (path, method)…
:: cqrs handlers: H | event listeners: L | scheduled tasks: S | controllers: C ::
:: openapi :: N operations | K component schemas (served at /v3/api-docs) ::
```

Reading it top to bottom:

- **`:: active profiles ::`** — the active config profiles (`default` when none
  is set).
- **`:: beans (N) ::`** — every bean the container scanned, one per line:
  `[stereotype]` (`service`, `repository`, `controller`, `configuration`,
  `component`, or `bean`), the bean name, its scope, and its short type name.
  This is the same table the admin dashboard's `/beans` view renders.
- **`:: routes (N) ::`** — the auto-mounted route table: each
  `#[rest_controller]` route as `METHOD path -> Controller::handler`. This is
  drawn from the same `firefly_container::routes()` registry that feeds
  `/admin/api/mappings` and the OpenAPI document, so the three never drift.
- **`:: cqrs handlers … ::`** — the *counts* drained from the inventory: how
  many `#[command_handler]`/`#[query_handler]`, `#[event_listener]`,
  `#[scheduled]`, and controllers were discovered.
- **`:: openapi ::`** — the operation count (one per route) and the component-
  schema count (one per `#[derive(Schema)]` DTO), confirming the spec is live.

Nothing in your app prints this — the framework does. The numbers are a quick
sanity check: if you expected four handlers and the report says three, a
`#[command_handler]` is missing or its crate is not linked.

## Graceful shutdown

`serve` starts the scheduler on a background task, then serves the public API on
`api_addr` and the management surface on `management_addr` through the framework
lifecycle, each with `with_graceful_shutdown`. On `SIGINT`/`SIGTERM` both
servers stop accepting connections, let in-flight requests finish, and `run`
returns `Ok(())` — a signal-triggered stop is a clean shutdown, not an error.

## The default 404

Because the framework installs a fallback, an unmatched path returns a proper
RFC 9457 `application/problem+json` 404 — the same `type`/`title`/`status`
envelope and `application/problem+json` content type as every other framework
error — instead of axum's bare empty body (which a browser would offer to
download as a blank file):

```text
GET /api/v1/nope

HTTP/1.1 404 Not Found
content-type: application/problem+json
{ "type": "...", "title": "Not Found", "status": 404,
  "detail": "No route matches GET /api/v1/nope" }
```

This is the same problem-rendering you met for handler errors in
[chapter 6](./06-first-http-api.md) and security errors in
[chapter 14](./14-security.md) — uniform errors, end to end, with no per-route
work.

## What changed in Lumen

Nothing was *added* here — this chapter explains the line that was already in
`main.rs` since the start. `FireflyApplication` is the spine the rest of the
book hangs on: every chapter that declares a bean, a controller, a handler, a
listener, or a scheduled task is contributing to the pipeline above, and this
chapter is where you see how the framework finds and wires it all from a single
line. Next, [Your First HTTP API](./06-first-http-api.md) writes the
controllers the framework auto-mounts.
