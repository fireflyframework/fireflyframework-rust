# Observability

In [Security](./14-security.md) you locked Lumen's mutating routes behind a JWT
and a role filter. The service is now safe — but it is still a black box. When a
deposit is slow in production you need to know *where* the time went; when the
broker degrades you want a dashboard that turns red before your pager does; when
an auditor asks why a transfer was rejected you need a structured log line with
enough context to reconstruct the decision.

The good news — and the theme of this whole chapter — is that almost none of
this is code you write. `FireflyApplication::run()` already installed the logging
layer, the health composite, the metric registry, the request-metrics
middleware, W3C trace-context origination, and a self-hosted `/admin` dashboard
bound to the live components. Lumen has been observable since
[Your First HTTP API](./06-first-http-api.md); this chapter teaches you to *read*
what is already there, and to add the handful of optional pieces — an info
contributor, a health probe, a domain metric, a custom dashboard view — that only
your application can supply.

By the end of this chapter you will:

- Reach Lumen's **management surface** — `/actuator/*` on the management port —
  and explain why it lives on a separate listener from the public API.
- Register an **info contributor** so `/actuator/info` reports which
  infrastructure this instance is running.
- Add a **health indicator** to the composite and watch it roll up into
  `/actuator/health`.
- Read the **request metrics** the framework already records, and record your own
  counter and gauge on the same registry.
- Understand **structured, correlation-enriched logging** and **W3C
  trace-context** — how one request stitches together across logs, events, and
  outbound calls with no manual threading.
- Open the **self-hosted admin dashboard**, read its fifteen views including the
  populated **Beans** view, and add a custom view of your own.

## Concepts you will meet

Each idea below is reintroduced in context where it is first used; this is the
short version so the vocabulary is in place before the first command.

> **Note** **Key term — management surface / actuator.** The *management surface*
> is a set of operational HTTP endpoints — health checks, build info, metrics,
> configuration introspection, runtime log-level control — that exist for
> operators and tooling, not for end users. Firefly serves them under
> `/actuator/*` on a **separate port** from your business API. This mirrors
> Spring Boot Actuator.

> **Note** **Key term — info contributor.** An *info contributor* is a small
> callback that adds a JSON section to `/actuator/info`. You register it on the
> application builder; the framework calls it when an operator hits the endpoint.
> The Spring analog is an `InfoContributor` bean.

> **Note** **Key term — health indicator and composite.** A *health indicator* is
> an async probe that reports `UP` / `DEGRADED` / `DOWN` (with an optional message
> and details). A *composite* aggregates many indicators into one rollup and
> serves it at `/actuator/health`. This is Spring Boot's `HealthIndicator` plus
> its health aggregator.

> **Note** **Key term — correlation id.** A *correlation id* is one identifier
> attached to everything a single request touches — every log line, every event
> it publishes, every outbound call it makes — so you can reconstruct the whole
> story from one value. Firefly sets it in a task-local scope on the way in; the
> Spring analog is an MDC entry threaded through a request.

## The two ports, and what each serves

Before the first endpoint, fix the mental model. Lumen runs **two listeners**:

- the **public API** on `0.0.0.0:8080` — your business routes and nothing else;
- the **management surface** on `0.0.0.0:8081` — `/actuator/*` plus the
  self-hosted `/admin` dashboard plus the auto-generated API docs.

`FireflyApplication` assembles and serves both routers; Lumen writes no actuator
or admin wiring at all. The split is the point: an operational endpoint like
`/actuator/env` (which can echo configuration) or `/admin` (a live dashboard)
never leaks onto the public network, because the public listener simply does not
mount those paths.

> **Note** **Key term — bind address override.** `FIREFLY_SERVER_ADDR` and
> `FIREFLY_MANAGEMENT_ADDR` are the two environment variables that move the public
> and management listeners independently (defaulting to `0.0.0.0:8080` /
> `0.0.0.0:8081`). You met them in [Quickstart](./02-quickstart.md); they are how
> you put the management port on a private interface in production.

> **Tip** **Checkpoint.** With Lumen running (`cargo run --bin lumen`), the
> management surface answers on `8081` and the public API on `8080`. Confirm the
> split: `curl localhost:8081/actuator/health` returns JSON, while
> `curl localhost:8080/actuator/health` returns a 404 problem document — the
> actuator is not on the public port.

## Step 1 — Reach the actuator

Even with no observability code of your own, the actuator is live. Start Lumen,
then from a second terminal walk three endpoints.

```bash
curl localhost:8081/actuator/health
# {"status":"UP", ...}

curl localhost:8081/actuator/info
# {"app":{"name":"lumen","version":"26.6.28"}, ...}

curl localhost:8081/actuator/metrics
# {"names":[ "http_server_requests_seconds", ... ]}
```

What just happened: `/actuator/health` aggregated every registered health
indicator into one `status`; `/actuator/info` echoed the app name you passed to
`FireflyApplication::new("lumen")` plus the framework version; `/actuator/metrics`
listed the meters the framework has been recording since boot — including the
per-route request timer you will read in Step 4.

The full management surface is below. Lumen's reaches the management port at
`http://localhost:8081/actuator/*`:

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
| `/actuator/beans`              | every DI bean (type, scope, stereotype, primary)   |
| `/actuator/mappings`           | every `#[rest_controller]` route (method/path)     |
| `/actuator/conditions`         | the conditional guards per bean                     |
| `/actuator/loggers[/:name]`    | runtime log-level control                          |
| `/actuator/threaddump`         | a thread/task dump                                  |
| `/actuator/httpexchanges`      | recent HTTP exchanges (when wired)                 |
| `/actuator/caches[/:name]`     | cache listing + eviction (when wired)              |
| `/actuator/refresh`            | reload config (the `Refresher` hook)               |

> **Note** The `beans` / `mappings` / `conditions` reports mirror Spring Boot
> Actuator's dependency-injection introspection — they are auto-registered by the
> framework alongside the rest, so you can introspect the wired object graph over
> HTTP without any app code. You saw the same inventory printed at boot in
> [Quickstart](./02-quickstart.md); these endpoints serve it live.

> **Tip** **Checkpoint.** All three `curl`s above return JSON. If `curl` connects
> but every path 404s, you are hitting `8080` (public) instead of `8081`
> (management). The public port has no `/actuator/*`.

## Step 2 — Describe this instance with an info contributor

`/actuator/info` already reports the `app` block — name and version — but it
cannot know what *infrastructure* this particular instance is running. That is
application knowledge, so it is the one piece of observability code Lumen could
add. You supply it as an **info contributor** registered fluently on the
application builder.

> **Note** **Key term — `InfoContributor`.** The type is
> `Box<dyn Fn() -> serde_json::Map<String, Value> + Send + Sync>` — a boxed
> closure that returns a JSON object. Each contributor's map becomes one section
> of `/actuator/info`. The closure runs on every request to the endpoint, so it
> can report live values.

```rust,ignore
use firefly::starter_core::InfoContributor;

let contributor: InfoContributor = Box::new(|| {
    let mut info = serde_json::Map::new();
    info.insert(
        "sample".into(),
        serde_json::json!({ "name": "lumen", "store": "in-memory", "eventBus": "in-memory" }),
    );
    info
});

firefly::FireflyApplication::new("lumen")
    .info_contributor(contributor)   // adds a "sample" section to /actuator/info
    .run()
    .await
```

What just happened, block by block:

- `InfoContributor` is re-exported through the facade at
  `firefly::starter_core::InfoContributor`, so Lumen still depends on only the one
  `firefly` crate.
- The closure builds a `serde_json::Map` with a single key, `sample`, whose value
  describes the store and event-bus kind this instance is running.
- `.info_contributor(contributor)` registers it on the builder. The framework
  threads every registered contributor into the `/actuator/info` handler when it
  builds the management router — no `actuator_router(..)` call and no
  second-listener bookkeeping in your code.

After this, `/actuator/info` reports both blocks:

```jsonc
// GET /actuator/info
{
  "app": { "name": "lumen", "version": "26.6.28" },
  "sample": { "name": "lumen", "store": "in-memory", "eventBus": "in-memory" }
}
```

The `app` block is filled from the `app_name` / `app_version` Lumen set in
[Configuration](./03-configuration.md); the `sample` block is the contributor
above. An operator hitting `/actuator/info` now sees at a glance that this
instance is on the in-memory infrastructure, not Postgres + Kafka.

> **Tip** **Checkpoint.** After adding the contributor and re-running,
> `curl localhost:8081/actuator/info` shows a top-level `sample` object reporting
> `"store":"in-memory"`. You can register more than one contributor; their maps
> are merged into the same JSON document.

## Step 3 — Add a health indicator

The composite that backs `/actuator/health` starts with the framework's own
indicators. A real Lumen deployment would add its own — a broker-liveness probe,
a store reachability check — so an orchestrator can tell a degraded instance from
a healthy one.

> **Note** **Key term — `IndicatorFn`.** `IndicatorFn::new(name, closure)` adapts
> a plain async closure into a health `Indicator`. The closure returns a
> `HealthResult` — `HealthResult::up()`, `HealthResult::degraded(msg)`, or
> `HealthResult::down(msg)`, each optionally enriched with `.with_detail(..)`. The
> composite rolls the results up: `DOWN` if any indicator is `DOWN`, else
> `DEGRADED` if any is `DEGRADED`, else `UP`.

The framework's `Core` already carries the `HealthComposite`. You bridge an
indicator onto it with `Core::add_observability_indicator(..)`. There are two
clean places to do it: declare the indicator as a `#[bean]` (the framework
discovers it during the component scan), or reach the composite from a
`FireflyApplication::on_ready` hook after the container is scanned. The hook form
looks like this:

```rust,ignore
use firefly::observability::{HealthResult, IndicatorFn};

// `core` is the wired Core (it owns the HealthComposite). Registering an
// indicator on it makes the probe appear under /actuator/health.
core.add_observability_indicator(IndicatorFn::new("event-bus", || async {
    HealthResult::up() // a real probe would ping the broker and return down() on failure
}));
```

What just happened: `IndicatorFn::new("event-bus", ..)` wraps the async closure
as an `Indicator` named `event-bus`; `add_observability_indicator` registers it
on the composite. The next `/actuator/health` call runs every indicator
concurrently and folds the results into the overall `status`, listing your probe
by name in the per-component breakdown.

> **Note** Health exposes two sub-paths your orchestrator's probes hit:
> `/actuator/health/liveness` and `/actuator/health/readiness`. They are separate
> so an in-flight migration that fails *readiness* (don't send me traffic yet)
> need not trip *liveness* (kill and restart me). Returning `degraded` keeps a
> probe `UP` while still flagging trouble on the rollup.

> **Tip** **Checkpoint.** After wiring an indicator, `curl
> localhost:8081/actuator/health` shows your probe's name alongside `status`. Make
> the closure return `HealthResult::down("broker unreachable")` once and watch the
> overall `status` flip to `DOWN` — that is the precedence rule in action.

## Step 4 — Read the request metrics you already have

You did not have to ask for per-route latency: request metrics are
auto-instrumented **on by default**, both at the `Core` layer (so even a bare
`Core` emits them) and through the web stack, which fills in a default
`RequestMetricsConfig` if you left one unset.

> **Note** **Key term — request metrics.** For every request the middleware
> records the labeled timer `http_server_requests_seconds` plus a companion
> `…_max` gauge, tagged `method` / templated `uri` (the matched route, so
> `/api/v1/wallets/:id` not the concrete id) / `status` / `outcome` /
> `exception`. A clean request carries `exception="None"`. This is the
> Micrometer/Spring Boot convention, so off-the-shelf scrapers read it unchanged.

Because the meter has been recording since the moment Lumen booted in
[Your First HTTP API](./06-first-http-api.md), this chapter only *exposes* it. Hit
a route a few times, then read the meter:

```bash
curl localhost:8080/api/v1/wallets/$ID            # generate some traffic
curl localhost:8081/actuator/metrics/http_server_requests_seconds
```

The dot-and-underscore meter name maps straight to Prometheus, so pointing a
Prometheus `scrape_config` at `/actuator/prometheus` lights up Grafana with no
extra code:

```bash
curl localhost:8081/actuator/prometheus | grep http_server_requests_seconds
```

> **Note** To turn the auto-instrumentation off, set
> `CoreConfig { disable_request_metrics: true, .. }`. To tune the rolling-max
> window or path exclusions instead of disabling, supply
> `request_metrics: Some(RequestMetricsConfig { .. })`. Both are configured the
> same way you configured everything else in [Configuration](./03-configuration.md).

### Recording your own meters

Beyond the request timer, you record domain meters on the **same** registry, so
they surface at `/actuator/metrics` and `/actuator/prometheus` immediately. Pull
the registry off the `Core` with `metric_registry()` (it is also a resolvable DI
bean you can `#[autowired]`), then create a counter or a gauge:

```rust,ignore
let metrics = core.metric_registry();

// A domain counter, bumped each time the transfer saga completes.
let transfers = metrics.counter("lumen_transfers_total");
transfers.inc();              // or transfers.add(3) for an explicit count

// A gauge sampling a live value (e.g. wallets currently held in the read model).
let active = metrics.gauge("lumen_wallets_active");
active.set(wallet_count as f64);
```

What just happened: `counter(name)` and `gauge(name)` return an
`Arc<Counter>` / `Arc<Gauge>` registered under that name. `Counter::inc()` adds
one (`add(n)` adds an explicit count); `Gauge::set(v)` records a sampled value.
Both meters now appear in the listing and the Prometheus scrape without any
exporter wiring.

> **Tip** **Checkpoint.** After incrementing `lumen_transfers_total` and reading
> `/actuator/metrics`, the meter listing includes `lumen_transfers_total`; the
> Prometheus scrape shows its current count. The registry is shared, so your
> domain meters and the framework's request timer live side by side.

## Step 5 — Structured logging and correlation

`FireflyApplication` installs a `tracing` layer that formats every event as one
structured line and enriches it with the request's correlation id (set by the
correlation middleware, on by default). It calls `init_logging` for you at boot —
best-effort, so a test harness that already owns the global subscriber does not
panic — and, with the `admin` feature on, tees the records into the dashboard's
live log buffer.

> **Note** **Key term — `init_logging`.** `init_logging(LogConfig)` installs the
> structured `tracing` subscriber as the global default. Its sibling
> `init_logging_with_layers([..])` does the same but stacks extra `tracing` layers
> over the correlation layer — the hook the admin dashboard uses to tee every log
> record into its in-memory buffer while the console JSON stream keeps flowing.
> You never call either yourself; the framework does it at boot.

```rust,ignore
// What FireflyApplication does at boot — Lumen writes none of this:
let _ = web.init_logging();
// (or web.init_logging_with_layers(vec![log_buffer]) when the admin tail is on)
```

After that, plain `tracing` macros produce enriched lines, and fields recorded on
an enclosing span merge into each event:

```rust,ignore
tracing::info!(wallet_id = %id, amount = %money, "deposit accepted");
```

The field names (`time`, `level`, `msg`, `service`, `correlationId`) follow a
stable, documented schema, so one log pipeline parses every Firefly service
consistently.

Because the correlation id lives in a task-local scope, it flows automatically
into every log line, every event Lumen's ledger publishes (`Event::new` stamps
it), and every outbound client call (the W3C `traceparent` is propagated). A
request that opens a wallet, publishes `WalletOpened`, and projects it into the
read model stitches together under one id with no manual threading — the
task-local correlation id stands in for the thread-local MDC plumbing you would
write by hand in other stacks.

### Configuring logging

Logging is configured the way you configure everything else — from the one main
config file. Bind the `firefly.logging.*` section into a `LogConfig` with
`firefly::observability::log_config_from_properties(props, base)`:

```yaml
firefly:
  logging:
    format: json                # json (default) | text (logfmt) | console
    level: info                 # root level
    level.firefly_web: warn     # per-logger levels (Spring's logging.level.<logger>)
    level.app::ledger: trace
    service: lumen              # the `service` field stamped on every line
    file:
      name: lumen.log           # enable the rolling file appender
      max-size: 10MB
      max-history: 7
```

What these keys do: `format` picks the output renderer; the bare `level` is the
root level, and `level.<target>` overrides one logger (matching Spring's
`logging.level.<logger>`); `service` is stamped on every line; the `file` block
switches on the rolling file appender and tunes its rotation. Per-logger levels,
the output format, and the rolling file appender therefore all come from config.
An external logging file can additionally be folded in with
`apply_external_config`.

And every level can be changed **without a restart** through
`POST /actuator/loggers/{name}` — the actuator's runtime logger control. The
endpoint reports each logger's `configuredLevel` / `effectiveLevel`, the
conventional shape management tooling expects:

```bash
# Raise app::ledger to TRACE on a running instance, no redeploy.
curl -X POST localhost:8081/actuator/loggers/app::ledger \
  -H 'content-type: application/json' \
  -d '{"configuredLevel":"TRACE"}'
```

> **Tip** **Checkpoint.** `curl localhost:8081/actuator/loggers` lists every
> logger with its `configuredLevel` / `effectiveLevel`. POST a new level to one
> logger, GET it back, and confirm the level changed on the live process.

## Step 6 — Trace context and OpenTelemetry

The default middleware chain `FireflyApplication` applies includes the
`TraceContextLayer`, which **originates** distributed trace context on every
request.

> **Note** **Key term — W3C trace context.** `traceparent` / `tracestate` are the
> standard HTTP headers that carry a distributed trace across service boundaries:
> a 32-hex trace-id and a 16-hex span-id identify where the request sits in a
> larger call tree. *Originating* means: when an inbound request carries no
> `traceparent`, the layer mints a fresh root span so the request still leaves
> Lumen as the head of a well-formed trace.

So the layer validates an inbound `traceparent` / `tracestate` when present and
mints a W3C root span when absent, inserts it into the request, and enriches every
log line with `trace_id` / `span_id`. A request that arrives with no trace header
still becomes the head of a distributed trace, and the `traceparent` Lumen
propagates on outbound calls becomes the parent/child edge to the next service.

The OpenTelemetry SDK wiring — exporters, sampling, resource attributes — is left
to your application, where you add your preferred OTel `tracing` layer alongside
the correlation layer. Lumen ships without an exporter (it is teaching code with
no external collector), but the trace-context origination and propagation are
already on the edges. When you do want spans flowing to a collector, build an OTLP
tracer and add `tracing-opentelemetry`'s layer to the subscriber Firefly
installed — the correlation layer keeps working alongside it:

```rust,ignore
use opentelemetry_otlp::WithExportConfig;
use tracing_subscriber::prelude::*;

// Build an OTLP pipeline pointing at your collector.
let tracer = opentelemetry_otlp::new_pipeline()
    .tracing()
    .with_exporter(
        opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint("http://otel-collector:4317"),
    )
    .install_batch(opentelemetry_sdk::runtime::Tokio)?;

// Register the OTel layer alongside Firefly's structured-log + correlation layers.
tracing_subscriber::registry()
    .with(tracing_opentelemetry::layer().with_tracer(tracer))
    .init();
```

The `traceparent` headers Firefly already propagates become the parent/child
edges between spans, so a request that fans out to an outbound call appears as a
single distributed trace in your backend.

## Step 7 — Global exception advice (optional)

Lumen's errors already render as RFC 9457 `application/problem+json` at the
handler boundary — you saw that from the very first endpoint in
[Your First HTTP API](./06-first-http-api.md). For a *cross-cutting* rewrite —
mapping a whole class of errors to a custom status or body without touching each
handler — the framework offers a transparent global advice layer.

> **Note** **Key term — global exception advice.** A registry of transforms that
> post-process every `application/problem+json` response after the handler
> produces it — the Rust analog of Spring's `@ControllerAdvice`. You register the
> registry as a `#[bean]`; the framework installs an `ExceptionAdviceLayer` as the
> outermost layer only when the registry is non-empty, so a service that declares
> no such bean keeps the plain RFC 9457 path.

Register an `ExceptionHandlerRegistry` bean and key transforms by problem type:

```rust,ignore
use firefly::web::ExceptionHandlerRegistry;
use firefly::kernel::{ProblemDetail, TYPE_NOT_FOUND};

// A #[bean] returning a registry: every "not found" becomes a friendlier 410.
#[bean]
fn exception_advice(&self) -> ExceptionHandlerRegistry {
    ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd: &ProblemDetail| {
        let mut out = pd.clone();
        out.status = 410;
        out
    })
}
```

What just happened: `on_type(TYPE_NOT_FOUND, transform)` registers a closure that
receives the produced `ProblemDetail` and returns a rewritten one — here flipping
the status from 404 to 410 (`Gone`). The framework runs the matching transform on
every outgoing problem document. Controller-local overrides still win over the
global rules, so a handler can opt out of the cross-cutting rewrite.

> **Tip** **Checkpoint.** With the bean registered, request a missing wallet and
> confirm the response status is now `410` while the body stays a valid
> `application/problem+json` document. Remove the bean and the status returns to
> the default `404` — proof the layer only installs when the registry is non-empty.

## Step 8 — The self-hosted admin dashboard

The actuator surface is JSON for machines. `firefly-admin` mounts a single-page
admin dashboard — vendored, no `npm` build — that ties health, metrics, loggers,
beans, mappings, caches, CQRS handlers, traces, and a live log tail into one pane
of glass with Server-Sent-Event streams.

> **Note** **Key term — self-hosted dashboard.** The dashboard is a vanilla-JS
> single-page app served by the framework itself on the management port — there is
> no separate frontend service to deploy and no build step. With the facade's
> `admin` feature enabled, `FireflyApplication` mounts it on `/admin/` and binds it
> to the live components.

With the `admin` feature on, **`FireflyApplication` self-hosts it on the
management port** and binds it to the real collaborators: the health composite, the
metric registry, the CQRS bus, the scheduler, the DI container (which backs the
Beans view), an environment snapshot built from the active profiles and the
`FIREFLY_*` process environment, a trace buffer fed by the HTTP-exchanges
recorder, and a log buffer fed by the tee'd logging layer. The `env` / `config` /
`mappings` panels show **real data**, not stubs. Lumen writes none of this wiring
— it ships the dashboard on `/admin/` simply by being a `FireflyApplication`.

```bash
cargo run --bin lumen --features admin
# then open http://localhost:8081/admin/ in a browser
```

The dashboard renders fifteen built-in views, grouped in the sidebar:

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
trace capture, so they never pollute the HTTP Traces panel.

### The Beans view

The **Beans** view is the dashboard's window onto the dependency-injection
container. Because `FireflyApplication` always passes the scanned container, the
dashboard serves:

| Endpoint                     | Returns                                              |
|------------------------------|------------------------------------------------------|
| `GET /admin/api/beans`       | every registered bean with its stereotype and scope  |
| `GET /admin/api/beans/graph` | the dependency graph between beans                   |
| `GET /admin/api/beans/:name` | one bean's detail (type, scope, dependencies)        |
| `GET /admin/api/sse/beans`   | a live snapshot at each refresh interval             |

The Overview view also rolls up a `beans` block (`{ total, stereotypes }`) and a
`wiring` block (live CQRS-handler and scheduled-task counts) drawn from the same
container, so the landing page shows how much the service is wired without opening
the full Beans view.

Lumen's Beans view is **populated**, not sparse: the framework component-scans the
configuration that declares Lumen's beans, so the event store, read model, query
cache, JWT service, the `FilterChain` / `BearerLayer`, the ledger application
service, and the `WalletApi` controller all appear as beans with their stereotypes
and the autowired dependencies between them. (Were you to host the dashboard
standalone without a container, these endpoints degrade gracefully to an empty
`{ "total": 0 }` block.)

> **Note** `firefly-admin` also runs in *server mode*: instances self-register
> through an admin client, and a central server aggregates a fleet of services
> into the Instances view. The dashboard is the same vanilla-JS SPA driven
> entirely by the `/admin/api` JSON + SSE endpoints — there is no frontend build
> step in either mode.

> **Tip** **Checkpoint.** With `--features admin`, open `http://localhost:8081/admin/`,
> select **Beans**, and find the `WalletApi` controller. Its autowired `bus` /
> `ledger` / `query_cache` dependencies should show as edges in the bean graph —
> proof the view is reading the real container, not a stub.

### A custom view

To add your own sidebar view, implement the `AdminView` trait and push it onto
`AdminDeps::views`; the dashboard lists it under `/admin/api/views[/:id]`. A Lumen
"Treasury" view surfaces the total custody balance across all wallets, queried
from the read model:

```rust,ignore
use std::sync::Arc;
use firefly::admin::AdminView;

struct TreasuryView {
    read_model: Arc<WalletReadModel>,
}

#[async_trait::async_trait]
impl AdminView for TreasuryView {
    fn view_id(&self) -> &str { "treasury" }
    fn display_name(&self) -> &str { "Treasury" }
    fn icon(&self) -> &str { "wallet" }

    // Backs GET /admin/api/views/treasury (keyed by view_id).
    async fn data(&self) -> serde_json::Value {
        let total: i64 = self.read_model.all().iter().map(|w| w.balance).sum();
        serde_json::json!({ "custodyTotal": total, "wallets": self.read_model.len() })
    }
}
```

The four trait methods are: `view_id` (the registry key and `/views/{id}` path
segment), `display_name` and `icon` (what the sidebar renders), and the async
`data()` that produces the view's JSON payload. Register the view before mounting
by pushing it onto `AdminDeps::views`:

```rust,ignore
let mut deps = AdminDeps::new(/* required collaborators … */);
deps.views.push(Arc::new(TreasuryView { read_model: Arc::clone(&read_model) }));
```

> **Note** When you let `FireflyApplication` self-host the dashboard you never
> build `AdminDeps` yourself — the framework sources every collaborator from the
> live web stack and the scanned container. You only construct `AdminDeps`
> directly in the advanced case below, where you host the dashboard outside a
> `FireflyApplication`.

> **Design note.** The dashboard router is reachable directly when you want to host
> it outside `FireflyApplication` — a custom server, or a test. `mount(AdminConfig,
> AdminDeps)` returns the router; `AdminDeps::new` takes the required collaborators
> and the rest are optional fields filled with struct-update syntax:
>
> ```rust,ignore
> use std::sync::Arc;
> use firefly::admin::{mount, AdminConfig, AdminDeps, LogBuffer, TraceBuffer};
>
> let deps = AdminDeps {
>     scheduler: Some(scheduler),    // → Scheduled Tasks view
>     bus: Some(bus),                // → CQRS view
>     container: Some(container),    // → Beans view
>     ..AdminDeps::new(
>         "lumen",
>         firefly::VERSION,
>         health_composite,          // Arc<HealthComposite>
>         metric_registry,           // Arc<MetricRegistry>
>         Arc::new(TraceBuffer::new()),
>         LogBuffer::new(),
>     )
> };
> let dashboard = mount(AdminConfig::default(), deps);
> ```
>
> `FireflyApplication` performs exactly this mount for you, which is why Lumen
> ships the dashboard with no admin code of its own.

## Recap — what changed in Lumen

This chapter taught you to read and extend Lumen's always-on observability
surface — all of it wired by `FireflyApplication`, with no observability code in
`main.rs`:

| Concern | Who wired it | Where you read / extend it |
|---------|--------------|----------------------------|
| Management surface on `:8081` | the framework | `curl /actuator/*`; override with `FIREFLY_MANAGEMENT_ADDR` |
| `/actuator/info` instance metadata | framework `app` block + your contributor | `.info_contributor(..)` on the builder |
| Health rollup | framework composite + your indicators | `add_observability_indicator(IndicatorFn::new(..))` |
| Request metrics (`http_server_requests_seconds`) | the framework, on by default | `/actuator/metrics`, `/actuator/prometheus` |
| Your domain meters | you, on the shared registry | `core.metric_registry().counter(..)` / `.gauge(..)` |
| Structured, correlation-enriched logs | `init_logging` at boot | plain `tracing` macros; tune via `firefly.logging.*` |
| W3C trace context | the `TraceContextLayer` | originated/propagated on the edges automatically |
| Self-hosted admin dashboard | `FireflyApplication` + the `admin` feature | `/admin/` — fifteen views including populated **Beans** |

You also now know that the correlation id flows automatically into every log
line, published event, and outbound call; that the `TraceContextLayer` originates
a W3C `traceparent` when one is absent; and that global exception advice is an
opt-in `#[bean]` the framework installs only when present.

## Exercises

1. **Reach the actuator.** Run `cargo run --bin lumen`, then `curl
   localhost:8081/actuator/info` and confirm the `sample` block reports the
   in-memory store. Hit `/actuator/health` and `/actuator/metrics`, then confirm
   `curl localhost:8080/actuator/health` returns a 404 problem — the actuator is
   not on the public port.
2. **Add a health indicator.** Wire an `IndicatorFn::new("read-model", ..)` onto
   the composite with `add_observability_indicator` (from an `on_ready` hook, or
   declare it as a `#[bean]`) that returns `UP` when the read model holds at least
   one wallet view and `DEGRADED` otherwise, then watch it appear under
   `/actuator/health`.
3. **A Lumen metric.** Record a counter — e.g. `lumen_transfers_total` — on
   `core.metric_registry()` each time the transfer saga completes, and verify it
   appears at `/actuator/metrics` and in the `/actuator/prometheus` scrape.
4. **Change a log level live.** `curl localhost:8081/actuator/loggers` to list the
   loggers, then `POST` a new `configuredLevel` to one of them and GET it back to
   confirm the change took effect on the running process — no restart.
5. **Explore the Beans view.** Run `cargo run --bin lumen --features admin`, open
   `http://localhost:8081/admin/`, and find the Beans view — note that it is
   *populated*. Locate the `WalletApi` controller and confirm its autowired `bus`
   / `ledger` / `query_cache` dependencies show in the bean graph.

## Where to go next

A service you can see is a service you can operate. The next chapter gives Lumen
work to do on its own — and a way to reach customers.

- Add background jobs and outbound notifications in
  **[Scheduling & Notifications](./16-scheduling-notifications.md)** — and watch
  the new `#[scheduled]` tasks appear under `/actuator/scheduledtasks` and the
  Scheduled Tasks dashboard view.
- Revisit how the framework discovers and wires the beans the **Beans** view
  shows in **[Dependency Wiring](./04-dependency-wiring.md)**.
- Drive the wired router — and assert on health and metrics — in tests with
  `bootstrap()` in **[Testing](./18-testing.md)**.
- Move the management port onto a private interface and turn on real
  infrastructure in **[Production & Deployment](./20-production.md)**.
