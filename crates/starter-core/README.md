# `firefly-starter-core`

> **Tier:** Starter · **Status:** Stable

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
  chain (problem renderer, correlation, idempotency).
* `actuator_router(info_contributors)` — pre-wired
  `/actuator/{health,info,metrics,env,tasks,version}` router.
* `new_application()` — `lifecycle::Application` named after the app.
* `print_banner()` — emits the ASCII banner identifying starter + app +
  runtime (`banner()` returns it as a `String` for tests).

### Optional middleware batteries (all OFF by default)

`CoreConfig` carries `Option`-typed knobs that — when set — weave
additional middleware surfaces into `apply_middleware` / `actuator_router`
at their canonical filter order. Leaving every knob unset (the default)
yields a Problem → Correlation → Idempotency chain and a minimal actuator
surface, so the simplest service stays lean.

| Knob | Effect when `Some` |
|------|--------------------|
| `cors` | `CorsLayer` at the outermost edge (preflight + simple-request decoration) |
| `security_headers` | `SecurityHeadersLayer` (OWASP response headers) |
| `csrf` | `CsrfLayer` (double-submit cookie) |
| `request_log` | `RequestLogLayer` (one structured access-log event per request) |
| `request_metrics` | `MetricsLayer` bridged into the actuator `MetricRegistry` via `MetricRegistryObserver` |
| `http_exchanges` | `HttpExchangesLayer` recording + `/actuator/httpexchanges` endpoint |
| `loggers` | `/actuator/loggers[/{name}]` runtime log-level control |
| `redaction` | PII scrubbing on the default log writer |

The effective `apply_middleware` chain (outermost → innermost) is:

```text
CorsLayer            (cors)              — CORS edge (preflight + simple)
ProblemLayer         (always)           — panic → 500 RFC7807
SecurityHeadersLayer (security_headers) — decorate every response
CorrelationLayer     (always)           — X-Correlation-Id
MetricsLayer         (request_metrics)  — http_server_requests_* (order -100)
HttpExchangesLayer   (http_exchanges)   — record into the recorder (order -90)
RequestLogLayer      (request_log)      — one access-log event (order +200)
CsrfLayer            (csrf)             — double-submit cookie (order +210)
IdempotencyLayer     (always)           — replay on Idempotency-Key (order +230)
        │
        ▼
     your router
```

The `firefly-web` `RequestObserver` trait is local on purpose (web does
not depend on the actuator); `MetricRegistryObserver` bridges it onto the
actuator `MetricRegistry`, and `firefly-starter-core` — the crate that
depends on both — is where that bridge lives. Each observation records the
labeled `http_server_requests_seconds` timer and the companion
`…_max` gauge, tagged `method`/`uri`/`status`/`outcome`/`exception`
(a clean request carries the `exception="None"` sentinel).

### Wiring a downstream admin dashboard

A downstream `firefly-admin` `AdminDeps` is built from the public `Core`
accessors — `cqrs_bus()`, `scheduler()`, `health_composite()`,
`metric_registry()`, `http_exchanges()`, `loggers()` — plus the public
fields they mirror. `firefly-starter-core` does **not** depend on
`firefly-admin` (a separate, later-tier crate), so there is no
`Core::admin_deps()` convenience: adding one would invert the dependency
graph (admin → starter-core, not the reverse). The admin crate constructs
its `AdminDeps` from a `&Core` (or a shared `Arc<Core>`) using these
accessors instead.

### Health glue

The actuator crate carries its own health primitives
(`HealthComposite` / `HealthIndicator`), so `Core` wires that type
directly. The `ObservabilityIndicator` bridge (and the
`core.add_observability_indicator(..)` convenience) adapts any
`firefly_observability::Indicator` onto the actuator composite, so
observability probes feed `/actuator/health` with the JSON wire shape
`status`, `message`, `details`, `duration` (in nanoseconds), and `time`.

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

    // optional middleware — all OFF (None) by default:
    pub cors: Option<CorsConfig>,                       // CorsLayer at the edge
    pub security_headers: Option<SecurityHeadersConfig>,// OWASP headers
    pub csrf: Option<CsrfLayer>,                        // double-submit cookie
    pub request_log: Option<RequestLogLayer>,          // access-log event
    pub request_metrics: Option<RequestMetricsConfig>, // http_server_requests_* bridge
    pub http_exchanges: Option<Arc<HttpExchangeRecorder>>, // recorder + endpoint
    pub loggers: Option<Arc<LoggersState>>,            // /actuator/loggers
    pub redaction: Option<RedactionConfig>,            // PII scrubbing on the log
}

pub struct RequestMetricsConfig {
    pub step_seconds: Option<f64>,            // rolling-max window (default 60s)
    pub exclude_patterns: Option<Vec<String>>,// path globs not instrumented
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

    // Accessors a downstream admin dashboard reads to build its AdminDeps:
    pub fn http_exchanges(&self) -> Option<Arc<HttpExchangeRecorder>>;
    pub fn loggers(&self) -> Option<Arc<LoggersState>>;
    pub fn cqrs_bus(&self) -> Arc<Bus>;
    pub fn scheduler(&self) -> Arc<Scheduler>;
    pub fn health_composite(&self) -> Arc<HealthComposite>;
    pub fn metric_registry(&self) -> Arc<MetricRegistry>;
}

pub struct ObservabilityIndicator { /* obs Indicator → actuator HealthIndicator */ }
pub struct MetricRegistryObserver { /* web RequestObserver → actuator MetricRegistry */ }
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

Coverage spans defaults wiring, the panic→500
`application/problem+json` middleware chain, the banner content, and
the full boot sequence (mount routers,
oneshot `/actuator/health`, dispatch a command, publish an event,
shut down through the lifecycle handle), validation middleware wired
by default, the cache DOWN → 503 path, the observability → actuator
health bridge, idempotency replay and correlation echo through the
middleware chain, and `/actuator/{version,info}` reflection.

The optional middleware wiring adds: every optional knob OFF by default (the
default chain unchanged), a headline boot test proving **CORS preflight +
security headers + a request-metrics counter incrementing** all flow
through `apply_middleware`, the metrics bridge tagging a panic as a 500
with `exception="panic"`, `/actuator/httpexchanges` recording and serving,
`/actuator/loggers` mounted only when wired, CSRF guarding unsafe requests,
idempotency replay surviving the full optional stack (proving the layer
order keeps idempotency innermost), and the `MetricRegistryObserver`
bridge recording the timer + max gauge directly.
