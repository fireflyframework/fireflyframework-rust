# `firefly-observability`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-otel-spring-boot-starter` · **Go module:** `observability` · **.NET project:** `FireflyFramework.Observability`

## Overview

`firefly-observability` provides three orthogonal concerns:

1. **Structured logging** — a [`tracing_subscriber`] layer
   (`CorrelationLayer`) that formats every event as one JSON (or logfmt
   text) line and auto-enriches it with the correlation id from the
   `firefly_kernel::with_correlation_id` task-local scope.
2. **Health indicators** — composable `Indicator` probes with a
   `Composite` aggregator producing the canonical UP / DEGRADED /
   DOWN / UNKNOWN rollup.
3. **Startup banner** — the ASCII Firefly banner + version + runtime
   identifying line.

OpenTelemetry SDK wiring (exporters, sampling, resource attributes)
is left to the application's `main.rs` — this crate exposes only the
building blocks that compose with the `tracing` ecosystem.

## Public surface

### Logging

```rust,ignore
pub enum LogFormat { Json, Text }            // Go: "json" | "text"
impl LogFormat { pub fn from_name(&str) -> Self; }

pub struct LogConfig {                       // Go: LogConfig
    pub level: tracing::Level,               //   Level   slog.Level
    pub service: String,                     //   Service string
    pub format: LogFormat,                   //   Format  string
}
impl Default for LogConfig {}                // Go: DefaultLogConfig() — JSON, info
impl LogConfig {                             // builder-style setters
    pub fn new() -> Self;
    pub fn with_level(self, Level) -> Self;
    pub fn with_service(self, impl Into<String>) -> Self;
    pub fn with_format(self, LogFormat) -> Self;
}

pub struct CorrelationLayer;                 // Go: CorrelationHandler, as a tracing Layer
impl CorrelationLayer {
    pub fn new(cfg: LogConfig) -> Self;                       // stdout
    pub fn with_writer(cfg, impl Write + Send + 'static) -> Self;  // Go: LogConfig.Output
}

pub fn subscriber(cfg) -> impl Subscriber + Send + Sync;      // Go: NewLogger(cfg)
pub fn subscriber_with_writer(cfg, writer) -> impl Subscriber + Send + Sync;
pub fn init_logging(cfg) -> Result<(), SetGlobalDefaultError>; // install globally

pub struct BufferWriter;                     // clonable in-memory sink (Go tests: bytes.Buffer)
```

Where Go wraps a `slog.Handler`, this port is a `tracing` `Layer` — but
the JSON log field names are identical to the Go `slog` JSON handler:
`time`, `level`, `msg`, `service`, `correlationId`, plus event fields at
top level. One log pipeline parses every port. Fields recorded on
enclosing `tracing` spans are merged into each event — the analog of
Go's `logger.With(...)`. `tracing`'s extra `TRACE` level maps to `DEBUG`
so the level vocabulary stays `DEBUG`/`INFO`/`WARN`/`ERROR` everywhere.

### Health

```rust,ignore
pub enum Status { Up, Down, Degraded, Unknown }  // serializes "UP" | "DOWN" | "DEGRADED" | "UNKNOWN"

pub struct HealthResult {                    // JSON shape identical to Go
    pub status: Status,                      //   "status"
    pub message: String,                     //   "message"  (omitted when empty)
    pub details: BTreeMap<String, Value>,    //   "details"  (omitted when empty)
    pub duration: std::time::Duration,       //   "duration" (integer nanoseconds)
    pub time: DateTime<Utc>,                 //   "time"     (RFC 3339)
}
impl HealthResult {
    pub fn up() -> Self;
    pub fn down(msg) -> Self;
    pub fn degraded(msg) -> Self;
    pub fn unknown() -> Self;
    pub fn with_message(self, msg) -> Self;
    pub fn with_detail(self, key, value) -> Self;
}

#[async_trait]
pub trait Indicator: Send + Sync {           // Go: Indicator
    fn name(&self) -> &str;
    async fn check(&self) -> HealthResult;   // ctx → cancellation via future drop
}

pub struct IndicatorFn<F>;                   // Go: IndicatorFunc
impl IndicatorFn { pub fn new(name, f: impl Fn() -> Future<HealthResult>) -> Self; }

pub struct Composite;                        // Go: Composite
impl Composite {
    pub fn new() -> Self;
    pub fn add(&self, impl Indicator + 'static);          // &self: interior mutability,
    pub fn add_arc(&self, Arc<dyn Indicator>);            // like Go's internal sync.RWMutex
    pub async fn check_all(&self) -> (Status, BTreeMap<String, HealthResult>);
}
```

The composite rollup is `DOWN` if any indicator is `DOWN`, else
`DEGRADED` if any is `DEGRADED`, else `UP`. `UNKNOWN` is neutral. Each
result is stamped with its check duration and UTC start time.

### Banner

```rust,ignore
pub struct BannerData { pub version, starter, app, rust_version: String }
pub fn print_banner(w: &mut impl Write, starter: &str, app: &str) -> io::Result<()>;
pub fn render_banner(w: &mut impl Write, data: BannerData) -> io::Result<()>;
pub fn banner_string(starter: &str, app: &str) -> String;
pub const RUSTC_VERSION: &str;               // Go: runtime.Version() minus the "go" prefix
```

Emits the ASCII art + framework version + runtime identifier. Called by
`firefly-starter-core` on startup. The template lives in
`crates/observability/banner.txt` (embedded via `include_str!`, the
analog of `go:embed`); the compiler version is captured by the build
script from `rustc --version`.

## Quick start

```rust
use firefly_observability::{
    init_logging, Composite, HealthResult, IndicatorFn, LogConfig, Status,
};

#[tokio::main]
async fn main() {
    // JSON logs to stdout, info level, `service:"orders"` on every record.
    init_logging(LogConfig::new().with_service("orders")).expect("install subscriber");

    firefly_kernel::with_correlation_id("abc-123", async {
        tracing::info!(id = "42", "placed order");
        // {"time":"…","level":"INFO","msg":"placed order","service":"orders","correlationId":"abc-123","id":"42"}
    })
    .await;

    let health = Composite::new();
    health.add(IndicatorFn::new("db", || async {
        match ping_db().await {
            Ok(()) => HealthResult::up(),
            Err(e) => HealthResult::down(e.to_string()),
        }
    }));
    let (overall, results) = health.check_all().await;
    assert_eq!(overall, Status::Up);
    assert_eq!(results["db"].status, Status::Up);
}

async fn ping_db() -> Result<(), std::io::Error> {
    Ok(())
}
```

The `firefly-actuator` crate mounts a composite like this on
`GET /actuator/health`.

## Adaptation notes (Go → Rust)

| Go | Rust |
|----|------|
| `slog.Handler` decorator (`CorrelationHandler`) | `tracing_subscriber` `Layer` (`CorrelationLayer`) |
| `kernel.CorrelationIDFrom(ctx)` | `firefly_kernel::correlation_id()` task-local read |
| `LogConfig.Output io.Writer` | `with_writer(cfg, impl Write + Send)` / `subscriber_with_writer` |
| `slog.LevelInfo` | `tracing::Level::INFO` (`TRACE` renders as `DEBUG`) |
| `logger.With(attrs...)` | fields on enclosing `tracing` spans |
| `Indicator.Check(ctx)` | `async fn check(&self)` — cancellation via future drop |
| `IndicatorFunc{NameValue, Fn}` | `IndicatorFn::new(name, async closure)` |
| `CheckAll` returning `map[string]HealthResult` | `BTreeMap<String, HealthResult>` (deterministic order) |
| `go:embed banner.txt` + `text/template` | `include_str!` + `{placeholder}` substitution |
| `runtime.Version()` minus `"go"` prefix | `RUSTC_VERSION` captured by `build.rs` from `rustc --version` |

## pyfly parity

The crate additionally ports the pyfly (`pyfly.observability` +
`pyfly.logging`) surface. Everything below is purely additive — every
Go-parity wire shape above is unchanged.

### Labeled metrics + `timed()` / `counted()` (pyfly `observability/metrics.py`)

```rust,ignore
pub struct MetricsRegistry;                  // pyfly: MetricsRegistry
impl MetricsRegistry {
    pub fn new() -> Self;                    // process-global, idempotent (pyfly module caches)
    pub fn isolated() -> Self;               // private registry (tests/exporters)
    pub fn counter(&self, name, desc, labels: &[&str]) -> Arc<Counter>;
    pub fn gauge(&self, name, desc, labels: &[&str]) -> Arc<Gauge>;
    pub fn histogram(&self, name, desc, labels: &[&str], buckets: Option<&[f64]>) -> Arc<Histogram>;
    pub fn prometheus_text(&self) -> String; // text exposition (counters as <name>_total)
}
// Counter/Gauge/Histogram: .labels(&["v", …]) -> Labeled* child series,
// inc/inc_by, set/add/inc/dec, observe; value()/value_with(), count()/sum().

pub async fn timed(®istry, name, fut) -> T;            // pyfly @timed
pub async fn timed_result(®istry, name, fut) -> Result<T, E>;
pub async fn counted(®istry, name, fut) -> T;          // pyfly @counted
pub async fn counted_result(®istry, name, fut) -> Result<T, E>;
pub struct Timed;   // builder: .description() .class() .method() .tag() .record()/.record_result()
pub struct Counted; // builder: same, counting result=success|failure + exception
```

Micrometer naming is preserved: `orders.process` → histogram
`orders_process_seconds` with `class`/`method`/`exception` labels;
counted meters are exposed as `<name>_total` with
`class`/`method`/`result`/`exception`. pyfly derives `class`/`method`
from the decorated function's qualname; in Rust they are explicit
builder fields (decorator → builder adaptation). The `exception` label
on `Err` is the unqualified error type name (`type(exc).__name__`
analog via `std::any::type_name`).

### W3C trace context (pyfly `observability/propagation.py` + `correlation.py`)

```rust,ignore
pub struct TraceParent { version, trace_id, parent_id, flags } // parse() / Display / sampled()
pub struct TraceState;                       // parse() / get() / entries() / Display
pub const TRACEPARENT_HEADER: &str;          // "traceparent"
pub const TRACESTATE_HEADER: &str;           // "tracestate"

pub async fn with_trace_context(tp: Option<String>, ts: Option<String>, fut) -> T;
pub fn current_traceparent() -> Option<String>;  // pyfly get_traceparent()
pub fn current_tracestate() -> Option<String>;   // pyfly get_tracestate()

pub struct TraceContextLayer;                // tower layer (pyfly TracingFilter):
                                             //   parses inbound headers, stores TraceParent/
                                             //   TraceState in request extensions + task-locals
pub fn inject_headers(&mut http::HeaderMap); // pyfly inject_headers (outbound)
pub fn inject_reqwest(reqwest::RequestBuilder) -> reqwest::RequestBuilder;
```

pyfly delegates to the OTel propagator; this port implements the W3C
wire format natively (lowercase hex, version `ff` and all-zero ids
rejected, future versions tolerated, `tracestate` capped at 32
members). The kernel task-local carries the correlation id; the
trace-context pair lives in this crate's own task-locals (pyfly
contextvars → tokio `task_local!`).

### Process metrics (pyfly `observability/process_metrics.py`)

```rust,ignore
pub struct ProcessMetricsCollector;          // sysinfo-backed
impl ProcessMetricsCollector {
    pub fn new() -> Self;
    pub fn uptime_seconds(&self) -> f64;     // process_uptime_seconds
    pub fn start_time_seconds(&self) -> f64; // process_start_time_seconds (real OS start time)
    pub fn cpu_count(&self) -> usize;        // system_cpu_count
    pub fn collect(&self, &MetricsRegistry); // refresh the three gauges
}
```

Micrometer/Spring Boot meter names, so Spring dashboards/alerts work
unchanged across ports.

### Per-target log levels + runtime `set_level` (pyfly level map / `LoggingPort`)

```rust,ignore
pub struct LogConfig {
    // … existing fields unchanged …
    pub levels: BTreeMap<String, Level>,     // pyfly {root: INFO, "my.module": DEBUG}
    pub file: Option<FileConfig>,            // pyfly.logging.file.*
    pub redaction: Option<RedactionConfig>,  // pyfly.logging.redaction.*
}
impl LogConfig {
    pub fn with_target_level(self, target, Level) -> Self; // "root" routes to .level
    pub fn with_file(self, FileConfig) -> Self;
    pub fn with_redaction(self, RedactionConfig) -> Self;
}

pub struct LevelHandle;                      // pyfly LoggingPort.set_level
impl LevelHandle {
    pub fn set_level(&self, target, Level);  // runtime change, "root" = root level
    pub fn clear_level(&self, target);       // drop an override (loggers POST null)
    pub fn level(&self, target) -> Level;    // effective (longest-prefix) level
    pub fn levels(&self) -> BTreeMap<String, Level>; // GET /actuator/loggers view
}
impl CorrelationLayer { pub fn level_handle(&self) -> LevelHandle; }

pub fn subscriber_with_handle(cfg) -> (impl Subscriber…, LevelHandle);
pub fn subscriber_with_writer_and_handle(cfg, writer) -> (impl Subscriber…, LevelHandle);
pub fn init_logging_with_handle(cfg) -> Result<LevelHandle, …>;
```

Targets match by longest prefix at a `::` (or `.`) boundary —
`firefly_web` covers `firefly_web::routes` but not `firefly_webx`.

### PII redaction (pyfly `logging/redaction/`)

```rust,ignore
pub trait Redactor { fn redact<'a>(&self, &'a str) -> Cow<'a, str>; }
pub struct RegexRedactor;                    // 10 builtin entities + extra patterns
pub enum MaskStyle { Placeholder, Partial, Hash } // <ENTITY> | ****1111 | <ENTITY:sha256_8>
pub struct RedactionConfig { enabled, entities, mask, extra_patterns, allow_fields, deny_fields }
pub fn build_redactor(&RedactionConfig) -> Option<RegexRedactor>;
pub fn luhn_valid(&str) -> bool;             // CREDIT_CARD validator
pub const BUILTIN_ENTITIES: [&str; 10];      // EMAIL, CREDIT_CARD, IBAN, US_SSN, JWT,
                                             // BEARER_TOKEN, URL_CREDENTIALS, PHONE, IPV4, IPV6
pub const REDACTED: &str;                    // "<REDACTED>" (deny-field replacement)
```

Wired into the JSON/text writers via `LogConfig::with_redaction`:
deny-listed keys are replaced wholesale, a non-empty allow list limits
scanning to listed fields plus the message, `CREDIT_CARD` matches are
gated by the Luhn check. The default config is `None`, so existing log
output is byte-identical unless redaction is opted in. Divergences
from pyfly, by design: the Presidio NER engine is Python-only (regex is
the cross-port contract; pyfly itself falls back to it), stdout/stderr
stream interception is not possible in Rust (redaction applies at the
layer's writer boundary), and the `PHONE` look-arounds are emulated
with a digit-boundary check (the `regex` crate has no look-around).

### Rolling file appender (pyfly `logging/handlers.py`)

```rust,ignore
pub fn parse_size("10MB" | "512KB" | "4096" | "") -> u64; // 0 when empty/invalid
pub struct FileConfig { name, path, max_size: String, max_history: u32 } // defaults: 10MB / 7
pub struct RollingFileWriter;                // impl Write; rotates app.log -> app.log.1…N,
                                             // prunes beyond max_history; 0 disables rotation
pub struct TeeWriter<A, B>;                  // console + file both receive every record
```

`LogConfig::with_file(FileConfig::new("app.log").with_path("logs"))`
tees output to console and file; an unopenable file falls back to
console only — a logging misconfiguration never crashes the
application.

## Testing

```bash
cargo test -p firefly-observability
```

Covers JSON-format correlation-id emission (sync and async task-local
scopes), the `degraded ⊕ up` overall computation, banner content and
overrides, plus Rust-specific cases: level filtering, text format, span
field merging, the Go JSON wire shape of `HealthResult` (nanosecond
`duration`, omitted empty `message`/`details`), and Send/Sync bounds.

The pyfly-parity surface is covered by `tests/pyfly_parity_test.rs`,
porting pyfly's `tests/observability/` (metric idempotency across
registries, `@timed`/`@counted` Micrometer naming and tags, W3C
inject/extract round trips, the tracing-filter inbound-trace test) and
`tests/logging/` (redaction engine/processor/patterns including Luhn,
`parse_size`, rotation + backup pruning, per-logger levels and runtime
`set_level`) suites.
