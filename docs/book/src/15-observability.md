# Observability

Firefly is **observable by default**: `Core::new` wires structured logging with
correlation enrichment, a health composite, a metrics registry, and the actuator
endpoints. This chapter covers the three orthogonal concerns of
`firefly-observability` — logging, health, and the banner — plus the
`firefly-actuator` management surface and the `firefly-admin` dashboard.

> **Spring parity** — This is `firefly-otel-spring-boot-starter` + Spring Boot
> Actuator: structured logs with MDC-style correlation, `/actuator/health` +
> `/actuator/metrics`, and a Spring-Boot-Admin-style dashboard.

## Structured logging

`firefly-observability` provides a `tracing` layer (`CorrelationLayer`) that
formats every event as one JSON (or text/console) line and auto-enriches it with
the correlation id from the kernel's task-local scope. `Core::init_logging`
installs it globally; the log field names (`time`, `level`, `msg`, `service`,
`correlationId`) are identical across the ports, so one pipeline parses
everything.

```rust,ignore
use firefly_observability::{init_logging, LogConfig, LogFormat};
use tracing::Level;

let cfg = LogConfig::new()
    .with_service("orders")
    .with_level(Level::INFO)
    .with_format(LogFormat::Json);  // Json | Text | Console (dev renderer)
init_logging(cfg)?;
```

After that, plain `tracing` macros produce enriched lines — and any fields you
record on an enclosing span are merged into each event:

```rust,ignore
use tracing::info;

let span = tracing::info_span!("create_order", order_id = "o1");
let _g = span.enter();
info!(customer = "alice", "order created"); // carries order_id + correlationId
```

The `Core` typically owns the `LogConfig` (preset to the app name); you usually
just call `core.init_logging()` and use `tracing` macros throughout.

## Correlation propagation

Because the correlation id lives in a task-local scope (set by the
`CorrelationLayer` HTTP middleware — see
[Your First HTTP API](./06-first-http-api.md)), it flows automatically into:

- every log line (via the logging layer above);
- every event you publish (`Event::new` stamps `correlationId`);
- every outbound client call (`RestClient` / `WebClient` propagate it as a
  header plus W3C `traceparent` / `tracestate`).

So a request that fans out to events and upstream calls stitches together under
one correlation id with no manual threading.

## Health

A health `Indicator` is an async probe returning a `HealthResult` (status +
message + details + duration + time). A `Composite` aggregates indicators into
the canonical rollup:

```rust,ignore
use firefly_observability::{Composite, HealthResult, IndicatorFn, Status};

let composite = Composite::new();
composite.add(IndicatorFn::new("db", || async {
    if db_reachable().await { HealthResult::up() } else { HealthResult::down("unreachable") }
}));

let (status, details) = composite.check_all().await;
assert!(matches!(status, Status::Up | Status::Degraded | Status::Down));
```

The rollup is `DOWN` if any indicator is `DOWN`, else `DEGRADED` if any is
`DEGRADED`, else `UP` (`UNKNOWN` is neutral). `Core` carries an actuator
`HealthComposite` with a default cache indicator; bridge an observability
`Indicator` onto it with `core.add_observability_indicator(..)`, and broker
liveness with the [`EventPublisherHealthIndicator`](./10-eda-messaging.md).

## The startup banner

`Core::print_banner()` emits the ASCII Firefly banner — the red script-figlet, a
`:: Firefly Framework for Rust ::  (v26.6.2)` tagline, the license line, and the
app / starter / runtime / active-profiles metadata. `core.banner()` returns it
as a `String` for tests.

## The actuator surface

`firefly-actuator` exposes the management endpoints, mounted by
`core.actuator_router(info_contributors)` — typically on a **separate listener**
so `/actuator/*` never leaks onto the public network:

| Endpoint                       | Returns                                            |
|--------------------------------|----------------------------------------------------|
| `/actuator/health`             | the composite rollup (+ liveness/readiness probes) |
| `/actuator/info`              | app metadata + your info contributors              |
| `/actuator/metrics`            | labeled Micrometer-parity metrics                  |
| `/actuator/env`               | masked, origin-attributed property sources         |
| `/actuator/tasks`             | scheduled-task descriptors                         |
| `/actuator/version`           | the running version                                |
| `/actuator/loggers[/{name}]`  | runtime log-level control (when wired)             |
| `/actuator/httpexchanges`     | recent HTTP exchanges (when wired)                 |
| `/actuator/threaddump`        | a thread/task dump                                 |
| `/actuator/refresh`           | reload config (the `ReloadableConfig` hook)        |

An info contributor adds a section to `/actuator/info`:

```rust,ignore
use firefly_starter_core::InfoContributor;

let contributor: InfoContributor = Box::new(|| {
    let mut m = serde_json::Map::new();
    m.insert("orders".into(), serde_json::json!({ "store": "in-memory" }));
    m
});
let admin = core.actuator_router(vec![contributor]);
```

## HTTP server metrics

When you set the `request_metrics` knob on `CoreConfig`, the `MetricsLayer`
records the labeled `http_server_requests_seconds` timer and a companion `_max`
gauge per request, tagged `method` / templated `uri` (the axum matched path) /
`status` / `outcome` / `exception`, and bridges them onto the actuator
`MetricRegistry` for `/actuator/metrics`. A clean request carries
`exception="None"`.

## Tracing / OpenTelemetry

The crate exposes the building blocks that compose with the `tracing`
ecosystem and propagates W3C trace-context (`traceparent` / `tracestate`) on the
HTTP edges and outbound client calls. OpenTelemetry SDK wiring — exporters,
sampling, resource attributes — is left to your `main.rs`, where you add your
preferred OTel `tracing` layer alongside the `CorrelationLayer`.

## The admin dashboard

`firefly-admin` mounts a Spring-Boot-Admin-style embedded dashboard: a
single-page UI (overview / health / metrics / loggers / mappings / caches /
scheduled tasks / traces / CQRS / transactions / beans / config / instances), a
JSON API over `firefly-actuator`, and SSE live streams. It builds its
`AdminDeps` from the public `Core` accessors (`cqrs_bus()`, `scheduler()`,
`health_composite()`, `metric_registry()`, `http_exchanges()`, `loggers()`), so
wiring it is a few lines once you have a `Core`.

With your service observable, the next chapter adds background work and outbound
messaging. Continue to [Scheduling & Notifications](./16-scheduling-notifications.md).
