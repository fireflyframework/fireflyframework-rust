# Observability

In Chapter 14 you locked Lumen's mutating routes behind a JWT and a role filter.
The service is now safe — but it is still a black box. When a deposit is slow in
production you need to know *where* the time went; when the broker degrades you
want a dashboard that turns red before your pager does; when an auditor asks why
a transfer was rejected you need a structured log line with the context to
reconstruct the decision.

By the end of this chapter Lumen will expose an **actuator admin surface** on a
separate port — health, info, metrics, loggers, scheduled tasks — feed it an
**info contributor** describing the build, emit **structured logs** with a
correlation id on every request, and (as the next step it grows into) surface all
of it in the embedded **admin dashboard** with its fifteen views, including the
new **Beans** view. Lumen is **observable by default**: `WebStack::new` already
wired the logging layer, the health composite, the metric registry, and the
request-metrics middleware. This chapter turns those defaults into an admin
surface and explains what each piece reports.

> **An `/actuator` management surface.** Firefly exposes a JSON management
> surface under `/actuator/*` — health, info, metrics, env, loggers, scheduled
> tasks — with structured, correlation-enriched logs and an embedded admin
> dashboard. The logger endpoint reports `configuredLevel` / `effectiveLevel`,
> the conventional shape standard metrics and management tooling expects, so
> off-the-shelf scrapers and dashboards work unchanged.

## The admin surface in main.rs

Lumen serves the public API on `127.0.0.1:8080` and the actuator on
`127.0.0.1:8081` — a separate listener, so `/actuator/*` never leaks onto the
public network. The `WebStack` `Deref`s to its `Core`, so `actuator_router` is
reachable directly. This is the relevant slice of `src/main.rs`:

```rust,ignore
use firefly::starter_core::InfoContributor;

let api = app.router();
let contributor: InfoContributor = Box::new(|| {
    let mut info = serde_json::Map::new();
    info.insert(
        "sample".into(),
        serde_json::json!({ "name": APP_NAME, "store": "in-memory", "eventBus": "in-memory" }),
    );
    info
});
let admin = app.web.actuator_router(vec![contributor]);
```

`actuator_router(info_contributors)` returns an axum `Router` carrying the full
management surface. Lumen mounts it on the admin listener in its lifecycle wiring
(the production chapter covers that end to end); for now, the two routers are
served on two ports.

## The actuator endpoints

`actuator_router` exposes the management endpoints below. Lumen's reach the admin
port at `http://127.0.0.1:8081/actuator/*`:

| Endpoint                       | Returns                                            |
|--------------------------------|----------------------------------------------------|
| `/actuator/health`             | the composite rollup (+ liveness/readiness probes) |
| `/actuator/info`               | app metadata + your info contributors              |
| `/actuator/metrics`            | the registered meter listing                       |
| `/actuator/metrics/:name`      | one meter's detail                                 |
| `/actuator/prometheus`         | the Prometheus text-exposition scrape target       |
| `/actuator/env`                | masked, origin-attributed property sources         |
| `/actuator/scheduledtasks`     | scheduled-task descriptors                         |
| `/actuator/version`            | the running version                                |
| `/actuator/loggers[/:name]`    | runtime log-level control                          |
| `/actuator/threaddump`         | a thread/task dump                                  |
| `/actuator/httpexchanges`      | recent HTTP exchanges (when wired)                 |
| `/actuator/caches[/:name]`     | cache listing + eviction (when wired)              |
| `/actuator/refresh`            | reload config (the `ReloadableConfig` hook)        |

### The info contributor

An `InfoContributor` is a boxed closure returning a `serde_json::Map`; each one
adds a section to `/actuator/info`. Lumen's reports its store and event-bus kind,
so an operator hitting `/actuator/info` sees that this instance is running the
in-memory infrastructure:

```jsonc
// GET /actuator/info
{
  "app": { "name": "lumen", "version": "26.6.4" },
  "sample": { "name": "lumen", "store": "in-memory", "eventBus": "in-memory" }
}
```

The `app` block is filled from the `CoreConfig.app_name` / `app_version` Lumen
set in Chapter 3; the `sample` block is the contributor above.

### Health

A health `Indicator` is an async probe returning a `HealthResult` (status +
message + details); a `Composite` aggregates them into the canonical rollup —
`DOWN` if any indicator is `DOWN`, else `DEGRADED` if any is `DEGRADED`, else
`UP`. The `Core` already carries a `HealthComposite`; bridge an observability
indicator onto it with `core.add_observability_indicator(..)`. A real Lumen
deployment would add a broker-liveness indicator and a store probe:

```rust,ignore
use firefly_observability::{HealthResult, IndicatorFn};

// Wire a probe onto the WebStack's composite (it Derefs to Core):
app.web.add_observability_indicator(IndicatorFn::new("event-bus", || async {
    HealthResult::up()   // a real probe would ping the broker
}));
```

`/actuator/health/liveness` and `/actuator/health/readiness` are the sub-paths
your orchestrator's probes hit — separate so an in-flight migration that fails
readiness need not trigger a liveness restart.

## Request metrics — already on

Request metrics are auto-instrumented **on by default** — at the `Core` layer
(so even a bare `Core` emits them) and through `WebStack::new`, which fills in a
`RequestMetricsConfig` if you left one unset. For every request the middleware
records the labeled `http_server_requests_seconds` timer plus a companion `_max`
gauge, tagged `method` / templated `uri` (the axum matched path, so
`/api/v1/wallets/:id` not the concrete id) / `status` / `outcome` / `exception`,
and bridges them onto the actuator `MetricRegistry`. A clean request carries
`exception="None"`. So the moment Lumen booted in Chapter 6 it was already
emitting per-route latency; this chapter just exposes it at `/actuator/metrics`.

> **Note.** To turn the auto-instrumentation off, set
> `CoreConfig { disable_request_metrics: true, .. }`; to tune the rolling-max
> window or path exclusions, supply a `request_metrics: Some(RequestMetricsConfig { .. })`.

The dot-case meter names map straight to Prometheus
(`http_server_requests_seconds`), so pointing a Prometheus `scrape_config` at
`/actuator/prometheus` lights up Grafana with no extra code.

Beyond the request timer, you record your own meters on the same registry. Pull
it off the `Core` with `metric_registry()`, then increment a counter or set a
gauge — both surface at `/actuator/metrics` and `/actuator/prometheus`
immediately:

```rust,ignore
let metrics = app.web.metric_registry();

// A domain counter, bumped each time the transfer saga completes.
metrics.counter("lumen_transfers_total").increment(1.0);

// A gauge sampling a live value (e.g. wallets currently held in the read model).
metrics.gauge("lumen_wallets_active").set(wallet_count as f64);
```

## Structured logging and correlation

`WebStack` installs a `tracing` layer that formats every event as one structured
line and enriches it with the request's correlation id (set by the correlation
middleware, on by default). Lumen calls `init_logging` once at startup, best-effort
so a test harness that already owns the global subscriber does not panic:

```rust,ignore
let app = build_app().await;
// Best-effort: a test harness may already own the global subscriber.
let _ = app.web.init_logging();
```

After that, plain `tracing` macros produce enriched lines; fields recorded on an
enclosing span merge into each event. The field names (`time`, `level`, `msg`,
`service`, `correlationId`) follow a stable, documented schema, so one log
pipeline parses every Firefly service consistently.

Because the correlation id lives in a task-local scope, it flows automatically
into every log line, every event Lumen's ledger publishes (`Event::new` stamps
it), and every outbound client call (the W3C `traceparent` is propagated). A
request that opens a wallet, publishes `WalletOpened`, and projects it into the
read model stitches together under one id with no manual threading.

> **Correlation flows automatically.** `init_logging` installs a structured
> `tracing` subscriber; the task-local correlation id is attached to every log
> line in place of manual thread-local plumbing, so a request stitches together
> across logs, events, and outbound calls with no field threading.

### Configuring logging

Logging is configured the way you configure everything else — from the one main
config file. Bind the `firefly.logging.*` section into a `LogConfig` with
`firefly_observability::log_config_from_properties(props, base)`:

```yaml
firefly:
  logging:
    level: info                 # root level
    format: json                # json | text (logfmt) | console
    level:
      firefly_web: warn         # per-logger levels (like logging.level.<logger>)
      app::ledger: trace
    file:
      name: lumen.log           # enable the rolling file appender
      max-size: 10MB
      max-history: 7
```

Per-logger levels, the output format, and the rolling file appender all come
from config; an external logging file can be folded in with
`apply_external_config`. And every level can be changed **without a restart**
through `POST /actuator/loggers/{name}` — the actuator's runtime logger control.

## Tracing / OpenTelemetry

`firefly-observability` exposes the building blocks that compose with the
`tracing` ecosystem and propagates W3C trace-context (`traceparent` /
`tracestate`) on the HTTP edges and outbound client calls. The OpenTelemetry SDK
wiring — exporters, sampling, resource attributes — is left to your `main.rs`,
where you add your preferred OTel `tracing` layer alongside the correlation
layer. Lumen ships without an exporter (it is teaching code with no external
collector), but the trace-context propagation is already on the edges.

When you do want spans flowing to a collector, build an OTLP tracer and add
`tracing-opentelemetry`'s layer to the subscriber Firefly installed — the
correlation layer keeps working alongside it:

```rust,ignore
use opentelemetry_otlp::WithExportConfig;
use tracing_subscriber::prelude::*;

// Build an OTLP pipeline pointing at your collector.
let tracer = opentelemetry_otlp::new_pipeline()
    .tracing()
    .with_exporter(opentelemetry_otlp::new_exporter().tonic().with_endpoint("http://otel-collector:4317"))
    .install_batch(opentelemetry_sdk::runtime::Tokio)?;

// Register the OTel layer alongside Firefly's structured-log + correlation layers.
tracing_subscriber::registry()
    .with(tracing_opentelemetry::layer().with_tracer(tracer))
    .init();
```

The `traceparent` headers Firefly already propagates become the parent/child
edges between spans, so a request that fans out to an outbound call appears as a
single distributed trace in your backend.

## The admin dashboard

The actuator surface is JSON for machines. `firefly-admin` mounts a single-page
admin dashboard — vendored, no `npm` build — that ties health, metrics, loggers,
beans, mappings, caches, CQRS handlers, sagas, traces, and a live log tail into
one pane of glass with Server-Sent-Event streams. It is the next surface Lumen grows into; you enable the facade's
`admin` feature and wire it from the `Core` accessors.

`mount(AdminConfig, AdminDeps)` returns the dashboard router. `AdminDeps::new`
takes the required collaborators; the rest are optional fields you fill in with
struct-update syntax. A Lumen wiring drawing on its `Core` looks like:

```rust,ignore
use std::sync::Arc;
use firefly::admin::{mount, AdminConfig, AdminDeps, LogBuffer, TraceBuffer};

let traces = Arc::new(TraceBuffer::new());
let logs = LogBuffer::new();

// `AdminDeps::new` takes the required collaborators; the optional fields that
// back the remaining views are set afterwards with struct-update syntax.
let deps = AdminDeps {
    scheduler: Some(app.web.scheduler()), // → Scheduled Tasks view
    bus: Some(app.web.cqrs_bus()),        // → CQRS view
    container: Some(container),           // → Beans view
    ..AdminDeps::new(
        APP_NAME,
        VERSION,
        app.web.health_composite(), // Arc<HealthComposite>
        app.web.metric_registry(),  // Arc<MetricRegistry>
        Arc::clone(&traces),
        logs,
    )
};

let dashboard = mount(AdminConfig::default(), deps);
```

The dashboard auto-discovers what those collaborators expose and renders it in
**fifteen built-in views**, grouped in the sidebar:

| Section        | Views                                                          |
|----------------|----------------------------------------------------------------|
| Dashboard      | Overview, Health                                               |
| Application    | **Beans**, Environment, Configuration, Loggers                 |
| Monitoring     | Metrics, Scheduled Tasks, HTTP Traces, Log Viewer              |
| Infrastructure | Mappings, Caches, CQRS, Transactions                           |
| Fleet          | Instances (server mode)                                        |

Each view is backed by a `/admin/api/*` JSON endpoint; the SSE streams
(`/admin/api/sse/{health,metrics,traces,logfile,beans,runtime,server}`) push
updates without your code polling. Admin and actuator paths are excluded from
trace capture so they never pollute the trace panel.

### The Beans view

The newest view is **Beans** — the dashboard's window onto the dependency-injection
container. When you pass `with_container(..)`, the dashboard serves:

| Endpoint                  | Returns                                                  |
|---------------------------|----------------------------------------------------------|
| `GET /admin/api/beans`       | every registered bean with its stereotype and scope     |
| `GET /admin/api/beans/graph` | the dependency graph between beans                      |
| `GET /admin/api/beans/:name` | one bean's detail (type, scope, dependencies)           |
| `GET /admin/api/sse/beans`   | a live snapshot at each refresh interval                 |

The Overview view also rolls up a `beans` block (`{ total, stereotypes }`) and a
`wiring` block (live CQRS-handler and scheduled-task counts) drawn from the same
container, so the landing page shows how much the service is wired without
opening the full Beans view. Because Lumen wires its composition root explicitly
rather than scanning a container, the Beans view is sparse for Lumen itself — but
the moment you adopt `#[derive(Component)]` + `firefly::scan` (Chapter 4), the
graph fills in. Without a container the endpoints degrade gracefully to an empty
`{ total: 0 }` block.

> **Beans and server mode.** The Beans view is the dashboard's window onto the
> DI container. `firefly-admin` also runs in *server mode* (`AdminServerConfig`):
> instances self-register through an `AdminClient`, and the server aggregates a
> fleet of services into the Instances view. The dashboard is a vanilla-JS SPA
> driven entirely by the `/admin/api` JSON + SSE endpoints — no frontend build
> step.

### Custom views

To add your own sidebar view, implement the `AdminView` trait and push it onto
`AdminDeps::views`; the dashboard lists it under `/admin/api/views[/:id]`. A
Lumen "Treasury" view surfaces the total custody balance across all wallets,
queried from the read model:

```rust,ignore
use firefly::admin::{AdminView, ViewMeta};

struct TreasuryView {
    read_model: Arc<WalletReadModel>,
}

#[async_trait::async_trait]
impl AdminView for TreasuryView {
    fn meta(&self) -> ViewMeta {
        ViewMeta { id: "treasury", title: "Treasury", section: "Infrastructure" }
    }

    // Backs GET /admin/api/views/treasury.
    async fn data(&self) -> serde_json::Value {
        let total: i64 = self.read_model.all().iter().map(|w| w.balance).sum();
        serde_json::json!({ "custodyTotal": total, "wallets": self.read_model.len() })
    }
}

// Register it before mounting:
let mut deps = AdminDeps::new(/* … */);
deps.views.push(Box::new(TreasuryView { read_model: Arc::clone(&read_model) }));
```

## What changed in Lumen

This chapter turned Lumen's always-on observability defaults into a working
admin surface:

- **`main.rs`** builds an `InfoContributor` describing the in-memory store and
  event bus, and serves `app.web.actuator_router(vec![contributor])` on the
  admin port — `/actuator/health`, `/info`, `/metrics`, `/loggers`,
  `/scheduledtasks`, and the rest.
- **`init_logging`** (best-effort, so the test harness can own the subscriber)
  switches on structured, correlation-enriched logging; the correlation id flows
  into every log line, published event, and outbound call automatically.
- The **request-metrics** middleware — on since `WebStack::new` — records
  `http_server_requests_seconds` per templated route, now exposed at
  `/actuator/metrics` and `/actuator/prometheus`.
- The **admin dashboard** (the `firefly-admin` step Lumen grows into) ties it all
  together in fifteen views, including the new **Beans** view backed by the DI
  container, with live SSE streams.

## Exercises

1. **Reach the actuator.** Run `cargo run --bin lumen`, then
   `curl http://127.0.0.1:8081/actuator/info` and confirm the `sample` block
   reports the in-memory store. Hit `/actuator/health` and `/actuator/metrics`.
2. **Add a health indicator.** Wire a `IndicatorFn::new("read-model", ..)` onto
   the composite with `add_observability_indicator` that returns `UP` when the
   read model holds at least one wallet view, and watch it appear under
   `/actuator/health`.
3. **A Lumen metric.** Record a counter — e.g. `lumen_transfers_total` — on the
   `metric_registry()` each time the transfer saga completes, and verify it
   appears at `/actuator/metrics`. (Recall the housekeeping heartbeat in
   Chapter 16 keeps an `AtomicU64` you could surface the same way.)
4. **Mount the dashboard.** Enable the facade's `admin` feature, mount the
   dashboard with `with_container`, and open `/admin` — note how the Beans view
   is sparse for Lumen's explicit root, then convert one collaborator to
   `#[derive(Component)]` and watch it appear.

With Lumen observable, the next chapter adds background work and the path to
outbound notifications. Continue to
[Scheduling & Notifications](./16-scheduling-notifications.md).
