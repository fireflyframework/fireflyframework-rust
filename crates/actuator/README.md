# `firefly-actuator`

> **Tier:** Platform · **Status:** Full · **Java original:** `spring-boot-starter-actuator` · **Go module:** `actuator`

## Overview

`firefly-actuator` exposes the framework's canonical **management endpoints**
as an axum `Router`:

| Endpoint                 | What it returns                                                                  |
|--------------------------|----------------------------------------------------------------------------------|
| `GET /actuator/health`   | Per-indicator + overall status; 200 UP/DEGRADED, 503 DOWN                        |
| `GET /actuator/info`     | Build info + app metadata + info contributors                                    |
| `GET /actuator/metrics`  | Prometheus exposition format from the `MetricRegistry` Counter / Gauge primitives|
| `GET /actuator/env`      | Redacted environment view (FIREFLY_* visible by default; everything else `***`)  |
| `GET /actuator/tasks`    | `{"count": N}` alive tokio tasks; `?dump=true` returns a runtime report          |
| `GET /actuator/version`  | `{"firefly":"26.6.1","app":"orders","appVersion":"…","rust":"…"}`                |

Bind these on a separate admin port (e.g. `:8081`) so they never leak
onto the public network.

## Why a separate crate?

Spring Boot's `actuator` is the canonical management surface every
operator expects — health probes for Kubernetes, metrics for
Prometheus, info for service catalogues. Without a unified
`actuator`, every Firefly service ends up handcrafting its own
`/healthz` with subtly different shapes. The JSON and Prometheus wire
formats here are identical to the Java, .NET, Go, and Python ports.

Two adaptations from the Go module:

- Go's `/actuator/goroutines` becomes `/actuator/tasks`: the same
  operational question ("how much concurrent work is in flight?")
  answered in the runtime's native unit — alive tokio tasks. Because
  async Rust has no `runtime.Stack` equivalent, `?dump=true` returns a
  plain-text tokio runtime report instead of per-task stack traces.
- Health indicators are **async**: the `HealthIndicator` trait is an
  `async_trait` port, and `IndicatorFn` adapts any async closure, just
  as `observability.IndicatorFunc` adapts plain functions in Go.

## Public surface

```rust,ignore
pub struct ActuatorConfig {
    pub app_name: String,
    pub app_version: String,                 // defaults to VERSION
    pub health: Arc<HealthComposite>,        // defaults to empty
    pub info_contributors: Vec<InfoContributor>, // merged into /info
    pub env_allow_prefixes: Vec<String>,     // defaults to ["FIREFLY_"]
    pub metric_registry: Arc<MetricRegistry>, // defaults to empty
}
pub fn mount(cfg: ActuatorConfig) -> axum::Router;

pub struct MetricRegistry { /* … */ }
impl MetricRegistry {
    pub fn new() -> Self;
    pub fn counter(&self, name: &str) -> Arc<Counter>;
    pub fn gauge(&self, name: &str) -> Arc<Gauge>;
    pub fn render(&self) -> String;          // Prometheus exposition text
}

impl Counter {
    pub fn inc(&self);                       // +1, lock-free
    pub fn add(&self, n: u64);
    pub fn get(&self) -> u64;
}

impl Gauge {
    pub fn set(&self, value: f64);
    pub fn get(&self) -> f64;
}
```

`Counter` and `Gauge` use `AtomicU64` internally (gauges store
`f64::to_bits`, the counterpart of Go's `math.Float64bits`) so
high-cardinality services never contend on metric writes.

## Quick start

```rust
use std::sync::Arc;
use firefly_actuator::{
    mount, ActuatorConfig, HealthComposite, HealthResult, IndicatorFn, MetricRegistry,
};

let health = Arc::new(HealthComposite::new());
health.add(IndicatorFn::new("db", || async { HealthResult::up() }));

let registry = Arc::new(MetricRegistry::new());
registry.counter("orders_placed_total").inc();
registry.gauge("queue_depth").set(42.5);

let app: axum::Router = mount(ActuatorConfig {
    app_name: "orders".into(),
    health,
    metric_registry: registry,
    info_contributors: vec![Box::new(|| {
        let mut m = serde_json::Map::new();
        m.insert("git".into(), serde_json::json!({ "sha": "abc" }));
        m
    })],
    ..ActuatorConfig::default()
});

// Serve on a dedicated admin port:
// axum::serve(tokio::net::TcpListener::bind("0.0.0.0:8081").await?, app).await?;
```

`firefly-starter-core` returns a pre-wired actuator router bound to the
core's health composite and metrics registry.

## Prometheus output sample

```
# TYPE orders_placed_total counter
orders_placed_total 3
# TYPE queue_depth gauge
queue_depth 42.500000
```

## pyfly parity

On top of the Go-parity surface above, the crate ports pyfly's
`actuator` package — Spring Boot's full management model:

| Endpoint                              | What it adds                                                                       |
|---------------------------------------|------------------------------------------------------------------------------------|
| `GET /actuator/health/liveness`       | Kubernetes liveness probe — only indicators tagged `ProbeGroup::Liveness`          |
| `GET /actuator/health/readiness`      | Kubernetes readiness probe — only `ProbeGroup::Readiness` indicators               |
| `GET /actuator/health/{group}`        | Named health group (registered via `HealthComposite::add_group`)                   |
| `GET /actuator/health/{component}`    | Per-component drill-down (200 UP, 503 DOWN, 404 unknown)                            |
| `GET/POST /actuator/loggers[/{name}]` | Runtime log levels over a `tracing_subscriber::reload::Handle<EnvFilter>`          |
| `GET /actuator/scheduledtasks`        | Tasks grouped by trigger (`cron` / `fixedDelay` / `fixedRate`)                     |
| `GET /actuator/caches[/{name}]`       | Configured caches; `POST /caches/{name}/evict` clears one                          |
| `POST /actuator/refresh`              | `{"refreshed": [keys…]}` from the wired `Refresher`                                |
| `GET /actuator/httpexchanges`         | The last 100 exchanges recorded by the `HttpExchangesLayer` ring buffer            |
| `GET /actuator/metrics/{name}?tag=k:v`| Micrometer JSON meter detail with `measurements` + `availableTags`                 |
| `GET /actuator/prometheus`            | Labeled Prometheus exposition (counters, gauges, histograms)                       |
| `GET /actuator/{id}[/{selector}]`     | Any custom `Endpoint` registered on the `EndpointRegistry`                         |

### Exposure model

`ExposureConfig` is the Spring `management.endpoints.web` model:
include/exclude id sets (CSV or `*` wildcard, exclude wins), a
configurable `base_path` (default `/actuator`), and per-endpoint
`endpoint_enabled` overrides. `mount()` honors it — an id is mounted
only when exposed and not disabled. The crate default exposes
everything (Go-parity backward compatibility);
`ExposureConfig::spring_default()` restores Spring's `health,info`.

```rust,ignore
// Probe groups + named groups
let health = Arc::new(HealthComposite::new());
health.add_with_groups(IndicatorFn::new("ping", || async { HealthResult::up() }),
                       &[ProbeGroup::Liveness]);
health.add(IndicatorFn::new("db", || async { HealthResult::up() }));
health.add_group("storage", &["db"]);

// Loggers over a real tracing reload handle
let (layer, handle) = tracing_subscriber::reload::Layer::new(EnvFilter::new("info"));
let loggers = Arc::new(LoggersState::from_handle_with_directives(handle, "info"));

// Labeled metrics + histograms (Micrometer JSON + Prometheus text)
let registry = Arc::new(MetricRegistry::new());
registry.counter_with("orders_total", &[("method", "GET")]).add(5);
registry.histogram("latency_seconds").observe(0.12);

let app = mount(ActuatorConfig {
    health,
    metric_registry: registry,
    loggers: Some(loggers),
    exposure: ExposureConfig::from_csv("*", "env"),
    ..ActuatorConfig::default()
});

// Record exchanges by layering the application router:
let recorder = Arc::new(HttpExchangeRecorder::new());
let app = app.layer(HttpExchangesLayer::new(Arc::clone(&recorder)));
```

`MetricRegistry` now carries labeled `Counter`/`Gauge` plus a
`Histogram` with fixed buckets (`DEFAULT_BUCKETS`) and a `TimerGuard`
that records an observation on drop. The Micrometer JSON view maps
counters to a `COUNT` statistic and histograms to
`COUNT`/`TOTAL_TIME`/`MAX`, exposes label values under `availableTags`,
and supports `?tag=k:v` filtering. `/actuator/prometheus` serves the
classic `version=0.0.4` text exposition with labels.

Custom endpoints implement the `Endpoint` trait (`id()` +
`handle(selector, query)`), register on an `EndpointRegistry`, and are
mounted at `{base_path}/{id}` — pyfly's `ActuatorRegistry`. The
`scheduledtasks`, `caches`, `refresh`, and `httpexchanges` surfaces are
wired through local traits (`ScheduledTasksSource`, `CacheOps`,
`Refresher`, `HttpExchangeRecorder`) so scheduling and caching stay
decoupled; the starter bridges them to the real subsystems.

## Testing

```bash
cargo test -p firefly-actuator
```

Covers UP / DEGRADED / DOWN status mapping (200 vs 503), info
contributor merging, env-prefix redaction, metrics formatting, the
task count + dump variants, and the version payload. The
`parity_test.rs` suite ports pyfly's `tests/actuator/` cases — probe
groups + isolation, named-group + component drill-down,
show-details/show-components switches, the exposure model
(include/exclude/base-path/per-endpoint enabled), custom endpoints,
loggers GET/POST over a real `EnvFilter` reload handle, scheduledtasks
grouping, caches + evict, refresh, httpexchanges header masking, and
the Micrometer JSON + labeled Prometheus views — all driven in-process
through `tower::ServiceExt::oneshot`, no sockets.
