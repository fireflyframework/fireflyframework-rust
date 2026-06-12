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

## Testing

```bash
cargo test -p firefly-actuator
```

Covers UP / DEGRADED / DOWN status mapping (200 vs 503), info
contributor merging, env-prefix redaction, metrics formatting, the
task count + dump variants, and the version payload — all driven
in-process through `tower::ServiceExt::oneshot`, no sockets.
