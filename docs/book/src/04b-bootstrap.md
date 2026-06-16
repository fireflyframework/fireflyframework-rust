# Bootstrapping with FireflyApplication

Lumen's `main` is a single line, and that line is the whole service. In
[Quickstart](./02-quickstart.md) you ran it and saw a banner, a startup report,
and two live ports — but you took `run()` on faith. This chapter opens the lid.
Nothing new gets *added* to Lumen here; instead you will learn exactly what
`FireflyApplication::new("lumen").run().await` does between the moment you press
Enter and the moment the two servers are accepting connections. Knowing the
pipeline pays dividends in every later chapter, because each one declares a bean,
controller, handler, listener, or scheduled task that *one of these stages*
discovers and wires for you.

By the end of this chapter you will:

- Explain the difference between `new`, `run`, and `bootstrap`, and know which
  one your tests should call.
- Walk the twelve-stage boot pipeline `bootstrap()` runs, and name what each
  stage discovers — the web stack, the DI scan, CQRS auto-configuration,
  security discovery, controller auto-mounting, handler draining, OpenAPI, and
  the management router.
- Use the `FireflyApplication` builder knobs (`version`, `configure`,
  `security`, `on_ready`, `extra_routes`, the address overrides) and know when
  the *declarative bean* path is preferred over the imperative knob.
- Override the public and management bind addresses from the environment with
  `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`.
- Read the line-by-line startup report and use it as a sanity check on what the
  framework wired.
- Understand graceful shutdown and the default RFC 9457 404 you get for free.

## Concepts you will meet

Before the pipeline walk, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — bootstrap.** *Bootstrapping* is the one-time act of
> assembling a running application from its declarations: building infrastructure,
> discovering components, wiring them together, and producing something
> serveable. In Firefly the entire bootstrap is the body of
> `FireflyApplication::bootstrap()`. The Spring analog is everything
> `SpringApplication.run(...)` does before the embedded server starts accepting
> requests.

> **Note** **Key term — composition root.** The *composition root* is the single
> place in a program where the object graph is assembled — where every component
> is constructed and connected. Many frameworks make you write this by hand. In
> Firefly the framework *is* the composition root: it scans your beans and wires
> them, so you never spell out the graph in a function. That is why Lumen has no
> `build_app`, no hand-written router, and no `register_*` call site.

> **Note** **Key term — inventory.** The *inventory* is a set of link-time
> registries the Firefly macros fill at compile time. When you write
> `#[command_handler]`, `#[event_listener]`, `#[scheduled]`, or
> `#[rest_controller]`, the macro registers the item into a global table that the
> framework *drains* at boot. There is no reflection and no runtime scanning of
> the filesystem: the declarations themselves *are* the registration. This is how
> `main` never changes as Lumen grows.

> **Note** **Key term — management surface.** The *management surface* is the set
> of operational HTTP endpoints — health, info, metrics, configuration
> introspection — plus the self-hosted admin dashboard and the API docs. Firefly
> serves them on a separate port (`8081` by default) from your business API
> (`8080`), so operational endpoints never leak onto the public network. This
> mirrors Spring Boot Actuator.

## Step 1 — Look at the one line you are about to decode

Lumen's `main` is the same one line you wrote in the quickstart, living in
`src/main.rs` beside the crate's `mod` declarations:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

What just happened: there is no `build_app`, no hand-written router, and no
`register_*` / `subscribe_*` / `schedule_*` call site anywhere in Lumen. `main`
only names the application and hands control to the framework. Everything else —
the beans from [Dependency Injection](./04a-dependency-injection.md), the
controllers from [Your First HTTP API](./06-first-http-api.md), the handlers from
[CQRS](./09-cqrs.md), the listeners from
[Event-Driven Architecture](./10-eda-messaging.md), and the scheduled tasks from
[Scheduling & Notifications](./16-scheduling-notifications.md) — is discovered by
the framework from declarations sitting next to the code.

> **Note** **Key term — `BoxError`.** `firefly::BoxError` is the framework's
> boxed error type, `Box<dyn std::error::Error + Send + Sync>`. Returning it from
> `main` lets you use `?` on the bootstrap and lets any startup failure surface as
> a non-zero process exit. It is re-exported from the `firefly` facade, so you
> never name the underlying crate.

> **Design note.** `FireflyApplication::new(name).run()` is the Rust analog of
> Spring Boot's `SpringApplication.run(App.class, args)` and pyfly's
> `FireflyApplication("lumen").run()`. That single call *is* the composition root.
> Nothing is reflective or hidden: the startup report (Step 10) logs exactly what
> was wired, so "what is running" is printed line-by-line at boot.

> **Tip** **Checkpoint.** Open `samples/lumen/src/main.rs` (or your own crate's
> `main.rs`). Confirm `main` is one statement: `new("lumen").run().await`. If you
> see a `build_app`, a router, or any `register_*` call, you are reading an older
> shape — the current framework wires all of that for you.

## Step 2 — Tell `new`, `run`, and `bootstrap` apart

Three methods drive the lifecycle, and choosing the right one is the difference
between a production server and a fast in-process test.

> **Note** **Key term — `bootstrap` vs `run`.** `bootstrap()` assembles the
> entire application — every stage in Step 4 — and returns a `Bootstrapped` value
> **without binding a socket or serving**. `run()` calls `bootstrap()` and then
> `serve()`. So the two paths assemble the *identical* app; only the last move
> (bind + serve) differs.

- **`FireflyApplication::new(name)`** constructs the builder. It reads the
  default bind addresses from the environment and seeds the app name. Nothing
  happens yet — no scan, no server.
- **`.run().await`** boots and serves until the process receives
  `SIGINT`/`SIGTERM`. This is what `main` calls.
- **`.bootstrap().await`** does everything `run` does *except* serve, returning a
  `Bootstrapped` whose `api_router` you can drive in-process. This is the test
  seam.

Lumen's HTTP tests use exactly the `bootstrap` path. Here is the real helper from
`src/web.rs` that the test modules call:

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

What just happened: `bootstrap()` returns a `Bootstrapped`, and `.api_router` is
its fully-assembled public router — controllers, middleware, and security all
applied. A test then drives that router with `tower::ServiceExt::oneshot`, sending
a request straight into the router with no TCP socket involved. Because the
production path (`run`) and the test path (`bootstrap` → `oneshot`) assemble the
**same** app, the tests in [Testing](./18-testing.md) exercise the exact wiring
that `main` serves.

> **Note** Notice this helper calls `.version(VERSION)` while `main` does not.
> The version is purely cosmetic — it shows up on the banner and in
> `/actuator/info` — so `main` can omit it and let it default. The test sets it
> explicitly only so assertions on `/actuator/info` are stable.

> **Tip** **Checkpoint.** You can now answer: *which method does a test call, and
> why?* A test calls `bootstrap()` because it wants the wired router without
> binding a port; `main` calls `run()` because it wants to serve.

## Step 3 — Meet the `Bootstrapped` value

`bootstrap()` hands back a `Bootstrapped` struct. You rarely construct one
yourself, but knowing its fields demystifies what "assembled" means. The real
shape from the framework:

```rust,ignore
pub struct Bootstrapped {
    /// The web stack (kept so `serve` can run the lifecycle).
    pub web: WebStack,
    /// The scanned DI container.
    pub container: Arc<Container>,
    /// The fully-assembled public API router (controllers + middleware + security).
    pub api_router: Router,
    /// The management router (`/actuator/*` + the self-hosted `/admin` dashboard).
    pub management_router: Router,
    /// The task scheduler (started by `serve`).
    pub scheduler: Arc<Scheduler>,
    /// The public bind address.
    pub api_addr: String,
    /// The management bind address.
    pub management_addr: String,
}
```

What just happened: a `Bootstrapped` carries both routers (public + management),
the scanned DI container, the scheduler that has not started yet, and the two
addresses to bind. `run()`'s only remaining job is to call `serve()` on this
value, which starts the scheduler and binds both routers. A test ignores
everything except `api_router`.

> **Note** **Key term — DI container.** The *container* is the registry that
> holds every bean the framework constructed, keyed by type, so any component can
> ask for a collaborator by type and get the managed instance. It is the runtime
> half of the dependency injection you met in
> [Dependency Injection](./04a-dependency-injection.md). `Bootstrapped.container`
> is that registry, fully scanned.

## Step 4 — Walk the boot pipeline, stage by stage

This is the heart of the chapter. `bootstrap()` runs a fixed pipeline, and every
stage ties to something the framework *discovers and wires* for you. Read it once
top to bottom; you will return to individual stages as later chapters add the
beans each stage finds. The numbering below follows the framework source
(`crates/firefly/src/application.rs`).

**1. Build the web stack.** `WebStack::new(config)` brings up axum, the CQRS
`Bus`, the EDA `Broker`, the `Scheduler`, the metric registry, the health
composite, and the default middleware (correlation id, request metrics,
idempotency, CORS, security headers). Security is *not* applied here — it comes
from a bean after the scan — so the stack is built plain and mutable.

> **Note** **Key term — bus / broker / scheduler.** The *bus* routes CQRS
> (Command/Query Responsibility Segregation) commands and queries to their
> handlers; the *broker* delivers events to listeners; the *scheduler* runs
> `#[scheduled]` tasks on a timer. All three are framework infrastructure beans,
> constructed here and registered into the container in stage 3 so your code can
> autowire them.

**2. Initialise logging.** The structured-logging subscriber is installed. When
the `admin` feature is on, logs are also teed into the admin dashboard's
in-memory capture buffer so `/admin` can show a live log tail.

**3. Component-scan the container.** The framework first **auto-registers its own
infrastructure beans** (`web.register_beans(&container)` — the `Bus`, `Broker`,
`Scheduler`, the registries), then `container.scan()` discovers, registers, and
autowires **your** beans — every `#[derive(Component/Service/Repository/
Configuration/Controller)]` and every `#[bean]` factory linked into the binary.
Immediately after the synchronous scan, `container.init_async_beans().await`
awaits every `async fn` `#[bean]` factory (a DB pool, a broker dial) so async
beans are live before anything resolves them — and a construction error aborts
startup (fail-fast). For Lumen that scan finds the `LumenBeans`
`#[derive(Configuration)]` and its `#[bean]` factories (the event store, query
cache, JWT service, `FilterChain`, `BearerLayer`, ledger) plus the `WalletApi`
controller bean. This is the link-time DI of
[Dependency Injection](./04a-dependency-injection.md).

**4. Auto-configure the CQRS bus.** Correlation propagation is always layered on
(`bus.use_middleware(CorrelationMiddleware::new())`). If a `QueryCache` bean is
present in the container, its read-cache middleware is layered on too — so
Lumen's `GetWallet` 30-second cache ([Caching](./17-caching.md)) is wired with no
app code, just by *declaring the `QueryCache` bean*. Validation middleware is
already installed by the core.

**5. Run the optional readiness hook.** Most apps — Lumen included — need none;
the beans and the pipeline cover the wiring. `on_ready` exists for the rare case
that wants the live collaborators (container, bus, broker, scheduler) after the
scan but before serving. We cover it in Step 5 below.

**6. Auto-discover security.** With no explicit `.security(...)` call, the
framework resolves the `FilterChain` bean (path-based RBAC) and the `BearerLayer`
bean (token extraction) from the container and applies them — Spring's
`SecurityFilterChain` bean, discovered. Lumen declares both as `#[bean]`s in
`LumenBeans`, so the secured routes from [Security](./14-security.md) light up
automatically. (If an `ExceptionHandlerRegistry` bean is present, it is installed
as the outermost advice layer — the `@ControllerAdvice` analog.)

**7. Auto-mount the routes.** `mount_controllers(&container)` resolves every
`#[rest_controller]` and builds its router from the controller's autowired state
bean; `mount_route_contributors(&container)` merges every `RouteContributor`
bean. This is how Lumen's feature-gated streaming endpoint is added — by
declaring a bean, not by editing a composition root. Resolving the controllers
here also constructs their collaborators, including the `ledger` `#[bean]`.

> **Note** **Key term — `RouteContributor`.** A `RouteContributor` is a bean that
> contributes raw axum routes the framework merges into the public router. It is
> the escape hatch for endpoints that do not fit the `#[rest_controller]` shape —
> like Lumen's reactive event stream. You declare it as a bean
> (`#[firefly(provides = "dyn firefly::web::RouteContributor")]`) and the
> framework finds it; there is still no composition root to edit.

**8. Drain the inventory.** The framework drains the link-time registries the
macros filled at compile time. `register_discovered_handlers(&bus)` plus
`register_discovered_handler_beans(&bus, &container)` install every
`#[command_handler]` / `#[query_handler]`;
`subscribe_discovered_listeners(broker)` plus the bean variant subscribe every
`#[event_listener]`; and `register_discovered_scheduled(&scheduler)` plus the
bean variant schedule every `#[scheduled]` task. There are no `register(&bus)` /
`subscribe(&broker)` call sites — the declarations *are* the registration.

**9. Apply the middleware chain.** The discovered middleware is applied over the
mounted routes, the bearer-auth layer is added, the default 404 fallback is set,
and `web.apply_middleware(...)` wraps the whole router in the inherited
observability edge — idempotency, the access log, request metrics, correlation,
W3C trace, security headers, problem rendering, CORS, and the global exception
advice. With the `admin` feature, an outermost trace layer **originates and
echoes** `traceparent` so every request is correlatable across services.

**10. Serve OpenAPI docs.** The spec is built from the **live inventory** — every
`#[rest_controller]` route plus every `#[derive(Schema)]` DTO — and served at
`/v3/api-docs` (plus `/openapi.json`), with Swagger UI at `/swagger-ui` and ReDoc
at `/redoc`. These are mounted on the **management** router (beside actuator and
admin), *not* the public API, since they expose the whole API surface. This is
auto-wired with no app code; [OpenAPI, Swagger UI & ReDoc](./06a-openapi.md)
covers it in full.

> **Note** The OpenAPI spec advertises the *public* API base URL as its `server`
> even though the docs are served on the management port — so Swagger UI's "Try
> it out" sends requests to `8080`, not the `8081` origin it loaded from.
> `FIREFLY_OPENAPI_SERVER_URL` overrides that base URL (for example, a public URL
> behind a reverse proxy).

**11. Install the default 404.** An unmatched route gets a proper RFC 9457
`application/problem+json` 404 instead of axum's bare empty body (see
[Step 8](#step-8--understand-the-default-404)).

**12. Build the management router.** The actuator endpoints
(`/actuator/health|info|metrics|loggers|mappings|beans|conditions|env`) are
assembled, and — with the `admin` feature — the **self-hosted admin dashboard**
is mounted at `/admin/`, wired to the live components (health, metrics, the bus,
the scheduler, the container, the environment snapshot, the trace buffer, the log
buffer). The OpenAPI docs router from stage 10 is merged in here, and a single
RFC 9457 404 fallback is set for the whole management surface.
[Observability](./15-observability.md) covers the admin surface in depth.

`bootstrap()` returns the assembled `Bootstrapped`; `run()` then calls `serve()`.

> **Tip** **Checkpoint.** Without re-reading, name what stage discovers a CQRS
> command handler (stage 8 — draining the inventory), a controller (stage 7 —
> auto-mounting), a security filter chain (stage 6 — security discovery), and the
> `GetWallet` read cache (stage 4 — CQRS auto-configuration, because a
> `QueryCache` bean is present). If you can place each, you understand why `main`
> never changes as Lumen grows.

## Step 5 — Reach for a builder knob (only when a bean will not do)

`FireflyApplication` is a builder, and every knob is optional. Lumen uses only
`new` (in `main`) and `version` (in `build_router`). Here is the full set, drawn
from the framework source so the signatures are exact:

| Method | What it does |
|--------|--------------|
| `new(name)` | Names the app (banner + `/actuator/info`). Defaults the binds from `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`. |
| `version(v)` | Sets the version (banner + `/actuator/info`). |
| `configure(\|cfg\| { … })` | Tunes the `CoreConfig` in place — CORS, security headers, idempotency, the knobs of [Configuration](./03-configuration.md). |
| `security(chain, bearer)` | Installs a `FilterChain` + `BearerLayer` **explicitly**, instead of discovering them from beans. |
| `on_ready(\|ctx\| async { … })` | A readiness hook over the live `container` / `bus` / `broker` / `scheduler`, run after the scan, before serving. |
| `extra_routes(\|container\| router)` | Merges extra non-`#[rest_controller]` routes built from the scanned container. |
| `info_contributor(c)` | Adds an `/actuator/info` contributor. |
| `api_addr(addr)` | Overrides the public API bind address. |
| `management_addr(addr)` | Overrides the management (actuator + admin) bind address. |
| `bootstrap()` | Assembles the app **without serving** (tests). |
| `run()` | Bootstraps and serves. |

The knobs are chainable. A hypothetical `orders` service that wants a touch of
imperative wiring might write:

```rust,ignore
// the knobs are chainable; Lumen needs almost none of them
firefly::FireflyApplication::new("orders")
    .version("1.0.0")
    .configure(|cfg| { /* tune the CoreConfig: CORS, security headers, … */ })
    .management_addr("127.0.0.1:9091")
    .run()
    .await
```

What just happened: each knob returns `Self`, so you chain as many as you need
and finish with `run()` (or `bootstrap()`). Most services finish with a much
shorter chain than this; Lumen's is the empty chain, `new("lumen").run()`.

> **Design note.** Lumen declares security as `#[bean]`s rather than calling
> `.security(...)`, declares its streaming endpoint as a `RouteContributor` bean
> rather than calling `.extra_routes(...)`, and seeds its projection inside the
> `ledger`/projection beans rather than in `.on_ready(...)`. The explicit builder
> knobs exist for apps that prefer a touch of imperative wiring; the *bean* route
> is the framework's preferred, fully-declarative path — declaration next to the
> code, discovered at boot. Prefer a bean; reach for a knob only when no bean
> shape fits.

> **Tip** **Checkpoint.** You can now justify why Lumen's `main` has no builder
> knobs at all: everything a knob could do, Lumen does with a bean the scan
> finds.

## Step 6 — Override the bind addresses from the environment

By default the public API binds `0.0.0.0:8080` and the management surface binds
`0.0.0.0:8081`. You can move either without touching code, because `new` reads
two environment variables at construction time:

```bash
FIREFLY_SERVER_ADDR=127.0.0.1:9090 \
FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091 \
cargo run --bin lumen
```

What just happened: `new` read `FIREFLY_SERVER_ADDR` for the public bind and
`FIREFLY_MANAGEMENT_ADDR` for the management bind, each falling back to its
`0.0.0.0:808x` default when unset. The two surfaces move *independently* — proof
that they are genuinely separate listeners, not one server with a path prefix.
If you would rather set the addresses in code, `api_addr(...)` /
`management_addr(...)` override the environment.

> **Tip** **Checkpoint.** Start Lumen with the two overrides above, then in a
> second terminal run `curl localhost:9091/actuator/health`. A `{"status":"UP"}`
> from `:9091` (and nothing on `:9090`'s `/actuator/*`) confirms the management
> surface moved on its own. This is the first taste of the typed configuration
> story in [Configuration](./03-configuration.md).

## Step 7 — Read the startup report

Just before serving, `serve()` prints the banner, the docs URLs, and then
`log_startup_report(&container)` — a Spring-Boot/pyfly-style **line-by-line
report** so a boot log reads like Spring Boot's console. The format is:

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
- **`:: beans (N) ::`** — every bean the container scanned, one per line: the
  `[stereotype]` (`service`, `repository`, `controller`, `configuration`,
  `component`, or `bean`), the bean name, its scope, and its short type name.
  This is the same table the admin dashboard's `/beans` view renders.
- **`:: routes (N) ::`** — the auto-mounted route table: each `#[rest_controller]`
  route as `METHOD path -> Controller::handler`. It is drawn from the same
  `firefly_container::routes()` registry that feeds `/admin/api/mappings` and the
  OpenAPI document, so the three never drift.
- **`:: cqrs handlers … ::`** — the *counts* drained from the inventory: how many
  `#[command_handler]`/`#[query_handler]`, `#[event_listener]`, `#[scheduled]`,
  and controllers were discovered (each count sums the free-`fn` and the bean
  registrations).
- **`:: openapi ::`** — the operation count (one per route) and the
  component-schema count (one per `#[derive(Schema)]` DTO), confirming the spec is
  live.

What just happened: nothing in your app printed this — the framework did, from
the live container and inventory. The numbers are a quick sanity check: if you
expected four handlers and the report says three, a `#[command_handler]` is
missing or its crate is not linked.

> **Tip** **Checkpoint.** Run `cargo run` and read the report. Note how short the
> `beans`, `routes`, and counts lines are today — Lumen has little business logic
> yet — then revisit this report after [CQRS](./09-cqrs.md) and watch the numbers
> grow without a single edit to `main`.

## Step 8 — Understand the default 404

Because the framework installs a fallback on both routers (stages 11 and 12), an
unmatched path returns a proper RFC 9457 `application/problem+json` 404 — the same
`type`/`title`/`status` envelope and `application/problem+json` content type as
every other framework error — instead of axum's bare empty body (which a browser
would offer to download as a blank file):

```text
GET /api/v1/nope

HTTP/1.1 404 Not Found
content-type: application/problem+json
{ "type": "...", "title": "Not Found", "status": 404,
  "detail": "No route matches GET /api/v1/nope" }
```

What just happened: the fallback is wired *inside* the observability edge, so even
an unmatched-route 404 is logged, traced, and correlated — there is no
observability gap for "the path that did not exist." This is the same
problem-rendering you meet for handler errors in
[Your First HTTP API](./06-first-http-api.md) and security errors in
[Security](./14-security.md) — uniform errors, end to end, with no per-route work.

> **Note** **Key term — RFC 9457.** RFC 9457 (which obsoletes RFC 7807) defines
> the `application/problem+json` media type: a small, machine-readable error
> envelope with `type`, `title`, `status`, and `detail` fields. Firefly renders
> *every* error — handler failures, validation, security, and unmatched routes —
> through this one shape, so a client parses errors exactly the same way no matter
> where they came from.

## Step 9 — Understand graceful shutdown

`serve()` starts the scheduler on a background task, then serves the public API on
`api_addr` and the management surface on `management_addr` through the framework
lifecycle, each wrapped with `with_graceful_shutdown`. On `SIGINT`/`SIGTERM` both
servers stop accepting new connections, let in-flight requests finish, and `run()`
returns `Ok(())`. A signal-triggered stop is treated as a *clean shutdown, not an
error* — the lifecycle's cancelled-error case is mapped to `Ok(())`.

What just happened: you never wrote a signal handler. The framework traps the
signal, drains both ports, and returns success, so a `Ctrl-C` at your terminal
exits with no stack trace and a zero exit code. That is the behaviour a container
orchestrator (Kubernetes sending `SIGTERM`) relies on for a rolling restart.

> **Tip** **Checkpoint.** Run Lumen, then press `Ctrl-C`. The process exits
> cleanly with no panic and no stack trace. If you saw an error, you are on an
> older build — the current `serve()` maps the cancellation to `Ok(())`.

## Recap

This chapter added no code to Lumen — it decoded the line that has been in
`main.rs` since the quickstart. You now know:

- **`new` / `run` / `bootstrap`.** `new` builds the builder; `run` bootstraps and
  serves; `bootstrap` assembles the identical app *without* serving and returns a
  `Bootstrapped` whose `api_router` your tests drive in-process.
- **The twelve-stage pipeline.** Build the web stack, init logging, component-scan
  the container (awaiting async beans), auto-configure the CQRS bus, run the
  optional readiness hook, auto-discover security, auto-mount controllers and
  route contributors, drain the inventory (handlers / listeners / scheduled
  tasks), apply the middleware chain, serve OpenAPI on the management port, install
  the default 404, and build the management router with actuator + admin.
- **Builder knobs vs beans.** `version`, `configure`, `security`, `on_ready`,
  `extra_routes`, `info_contributor`, and the address overrides exist for
  imperative wiring — but Lumen prefers the declarative bean for every one of
  them, so its `main` is the empty chain.
- **The operational defaults.** Two independent ports overridable by
  `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`, a line-by-line startup report,
  an RFC 9457 404 for unmatched paths, and graceful SIGINT/SIGTERM shutdown — all
  for free.

`FireflyApplication` is the spine the rest of the book hangs on. Every chapter
that declares a bean, a controller, a handler, a listener, or a scheduled task is
contributing to the pipeline above — and never by rewriting `main`, only by giving
the framework one more thing to discover.

## Exercises

1. **Trace a stage to a chapter.** For each of stages 4, 6, 7, and 8, name the
   later chapter that adds the bean or declaration that stage discovers, and the
   single line of Lumen code that makes it light up. (Hint: stage 4 is the
   `QueryCache` bean from [Caching](./17-caching.md).)
2. **Drive the test seam.** In `samples/lumen`, read `src/http_test.rs` and find
   where it calls `build_router()`. Confirm the test never binds a socket — it
   drives the `bootstrap()`-assembled router directly. Then explain why a passing
   HTTP test proves something about the *production* `run()` path.
3. **Move the ports independently.** Start Lumen with
   `FIREFLY_SERVER_ADDR=127.0.0.1:9090 FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091
   cargo run`, then `curl localhost:9091/actuator/health` and
   `curl localhost:9090/api/v1/wallets/none`. Confirm health answers on `:9091`
   and the public 404 (RFC 9457 problem+json) answers on `:9090`.
4. **Read the startup report as a checklist.** Run Lumen and copy the
   `:: cqrs handlers … ::` and `:: routes … ::` lines. After you finish
   [CQRS](./09-cqrs.md), run it again and diff the two — every new number should
   map to a `#[command_handler]`, `#[query_handler]`, or `#[rest_controller]` you
   added, with `main` untouched.
5. **Provoke graceful shutdown.** Run Lumen, fire a slow request, and press
   `Ctrl-C` mid-flight. Confirm the in-flight request still completes and the
   process exits with code `0` and no stack trace — the signal was a shutdown, not
   a failure.

## Where to go next

- See the beans this pipeline scans declared in
  **[Dependency Injection & Auto-Configuration](./04a-dependency-injection.md)**.
- Write the `#[rest_controller]` that stage 7 auto-mounts in
  **[Your First HTTP API](./06-first-http-api.md)**.
- Watch the OpenAPI spec stage 10 builds come to life in
  **[OpenAPI, Swagger UI & ReDoc](./06a-openapi.md)**.
