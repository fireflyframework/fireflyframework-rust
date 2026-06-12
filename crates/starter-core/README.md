# `firefly-starter-core`

> **Tier:** Starter · **Status:** Full · **Java original:** `firefly-starter-core` · **Go module:** `startercore`

## Overview

`firefly-starter-core` is the **one-call infrastructure-tier wiring** for
any Firefly Rust service. A single `Core::new(CoreConfig { .. })` returns
a `Core` struct holding every component a typical service needs:

* `log` — `LogConfig` with correlation-id enrichment, pre-set to the app
  name (install it globally with `core.init_logging()`).
* `cache` — `Arc<dyn cache::Adapter>`, default `MemoryAdapter`.
* `bus` — `Arc<cqrs::Bus>` with the `ValidationMiddleware` pre-installed.
* `broker` — `Arc<dyn eda::Broker>`, default `InMemoryBroker`.
* `health` — `Arc<actuator::HealthComposite>` with a default cache
  health indicator.
* `idempotency` — `web::IdempotencyConfig`.
* `metrics` — `Arc<actuator::MetricRegistry>`.
* `scheduler` — `Arc<scheduling::Scheduler>`.

Plus four convenience methods:

* `apply_middleware(router)` — the canonical outermost HTTP middleware
  chain (problem renderer, correlation, idempotency) — Go's
  `Middleware()`.
* `actuator_router(info_contributors)` — pre-wired
  `/actuator/{health,info,metrics,env,tasks,version}` router — Go's
  `ActuatorHandler(infoContributors...)`.
* `new_application()` — `lifecycle::Application` named after the app.
* `print_banner()` — emits the ASCII banner identifying starter + app +
  runtime (`banner()` returns it as a `String` for tests).

### Health glue

The Go module hands an `observability.Composite` to `actuator.Mount`;
the Rust actuator crate carries its own health primitives
(`HealthComposite` / `HealthIndicator`), so `Core` wires that type
directly. The `ObservabilityIndicator` bridge (and the
`core.add_observability_indicator(..)` convenience) adapts any
`firefly_observability::Indicator` onto the actuator composite, so
observability probes feed `/actuator/health` exactly as in Go — both
sides emit the identical JSON wire shape (`status`, `message`,
`details`, `duration` in nanoseconds, `time`).

## Public surface

```rust,ignore
pub struct CoreConfig {
    pub app_name: String,                       // default "firefly-app"
    pub app_version: String,
    pub starter_name: String,                   // default "starter-core"
    pub log: Option<LogConfig>,                 // default JSON/info, service = app
    pub cache: Option<Arc<dyn Adapter>>,        // default MemoryAdapter
    pub bus: Option<Arc<Bus>>,                  // default Bus::new()
    pub broker: Option<Arc<dyn Broker>>,        // default InMemoryBroker
    pub health: Option<Arc<HealthComposite>>,   // default empty composite
    pub idempotency: Option<IdempotencyConfig>, // default 24 h, POST/PUT/PATCH
    pub metrics: Option<Arc<MetricRegistry>>,   // default empty registry
    pub scheduler: Option<Arc<Scheduler>>,      // default Scheduler::new()
}

pub struct Core { /* every field above, defaulted and wired */ }
impl Core {
    pub fn new(cfg: CoreConfig) -> Self;
    pub fn apply_middleware(&self, router: axum::Router) -> axum::Router;
    pub fn actuator_router(&self, info_contributors: Vec<InfoContributor>) -> axum::Router;
    pub fn new_application(&self) -> Application;
    pub fn init_logging(&self) -> Result<(), SetGlobalDefaultError>;
    pub fn add_observability_indicator(&self, indicator: impl Indicator + 'static);
    pub fn banner(&self) -> String;
    pub fn print_banner(&self);
}

pub struct ObservabilityIndicator { /* obs Indicator → actuator HealthIndicator */ }
pub fn to_actuator_status(s: firefly_observability::Status) -> HealthStatus;
pub fn to_actuator_result(r: firefly_observability::HealthResult) -> HealthResult;
```

The component types (`Bus`, `Adapter`, `MemoryAdapter`, `Broker`,
`InMemoryBroker`, `HealthComposite`, `MetricRegistry`, `Scheduler`,
`Application`, `IdempotencyConfig`, the web layers, …) are re-exported
flat from this crate, so a service can depend on `firefly-starter-core`
alone.

## Quick start

```rust,ignore
use axum::{routing::get, Router};
use firefly_starter_core::{Core, CoreConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core = Core::new(CoreConfig {
        app_name: "orders".into(),
        app_version: "1.0.0".into(),
        ..CoreConfig::default()
    });
    core.init_logging()?;
    core.print_banner();

    let api = core.apply_middleware(Router::new().route("/orders", get(|| async { "[]" })));
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
    app.run().await?; // blocks until ctrl-c / SIGTERM / handle.shutdown()
    Ok(())
}
```

## Testing

```bash
cargo test -p firefly-starter-core
```

Ports every Go test (defaults are wired, the panic→500
`application/problem+json` middleware chain, the banner content) and
adds Rust-specific coverage: the full boot sequence (mount routers,
oneshot `/actuator/health`, dispatch a command, publish an event,
shut down through the lifecycle handle), validation middleware wired
by default, the cache DOWN → 503 path, the observability → actuator
health bridge, idempotency replay and correlation echo through the
middleware chain, and `/actuator/{version,info}` reflection.
