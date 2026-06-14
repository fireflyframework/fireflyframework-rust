# `firefly-observability`

> **Tier:** Platform · **Status:** Stable

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
pub enum LogFormat { Json, Text, Console }   // "json" | "text"; +Console (dev renderer)
impl LogFormat { pub fn from_name(&str) -> Self; }  // console|pretty|dev -> Console; logfmt|text -> Text

pub struct LogConfig {
    pub level: tracing::Level,
    pub service: String,
    pub format: LogFormat,
}
impl Default for LogConfig {}                // JSON, info
impl LogConfig {                             // builder-style setters
    pub fn new() -> Self;
    pub fn with_level(self, Level) -> Self;
    pub fn with_service(self, impl Into<String>) -> Self;
    pub fn with_format(self, LogFormat) -> Self;
}

pub struct CorrelationLayer;                 // a tracing Layer
impl CorrelationLayer {
    pub fn new(cfg: LogConfig) -> Self;                       // stdout
    pub fn with_writer(cfg, impl Write + Send + 'static) -> Self;  // custom output writer
}

pub fn subscriber(cfg) -> impl Subscriber + Send + Sync;
pub fn subscriber_with_writer(cfg, writer) -> impl Subscriber + Send + Sync;
pub fn init_logging(cfg) -> Result<(), SetGlobalDefaultError>; // install globally

pub struct BufferWriter;                     // clonable in-memory sink (for tests)
```

`CorrelationLayer` is a `tracing_subscriber` `Layer`. The JSON log field
names are `time`, `level`, `msg`, `service`, `correlationId`, plus event
fields at top level — a stable wire shape designed so a single log pipeline
parses every service uniformly. Fields recorded on enclosing `tracing` spans
are merged into each event. `tracing`'s extra `TRACE` level maps to `DEBUG`
so the level vocabulary stays `DEBUG`/`INFO`/`WARN`/`ERROR` everywhere.

**Log ↔ trace correlation.** When a W3C trace context is in scope (set by
`TraceContextLayer` / `with_trace_context`), every record also carries
`trace_id` (32-hex) and `span_id` (16-hex), sourced from the active
`traceparent`, so logs and traces join in the same pipeline. A no-op when
no trace context is set.

### Health

```rust,ignore
pub enum Status { Up, Down, Degraded, Unknown }  // serializes "UP" | "DOWN" | "DEGRADED" | "UNKNOWN"

pub struct HealthResult {
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
pub trait Indicator: Send + Sync {
    fn name(&self) -> &str;
    async fn check(&self) -> HealthResult;   // cancellation via future drop
}

pub struct IndicatorFn<F>;
impl IndicatorFn { pub fn new(name, f: impl Fn() -> Future<HealthResult>) -> Self; }

pub struct Composite;
impl Composite {
    pub fn new() -> Self;
    pub fn add(&self, impl Indicator + 'static);          // &self: interior mutability
    pub fn add_arc(&self, Arc<dyn Indicator>);
    pub async fn check_all(&self) -> (Status, BTreeMap<String, HealthResult>);
}
```

The composite rollup is `DOWN` if any indicator is `DOWN`, else
`DEGRADED` if any is `DEGRADED`, else `UP`. `UNKNOWN` is neutral. Each
result is stamped with its check duration and UTC start time.

### Banner

The banner renders the red `firefly` script-figlet, a
`:: Firefly Framework for Rust ::  (v<version>)` tagline,
`(c) 2026 Firefly Software Foundation`, `Licensed under Apache 2.0`, then
app / starter / runtime / active-profiles metadata and an optional
Swagger-UI URL line.

Two layers of API:

```rust,ignore
// Simple, always-plain (no ANSI).
pub struct BannerData { pub version, starter, app, rust_version: String }
pub fn print_banner(w: &mut impl Write, starter: &str, app: &str) -> io::Result<()>;
pub fn render_banner(w: &mut impl Write, data: BannerData) -> io::Result<()>;
pub fn banner_string(starter: &str, app: &str) -> String;
pub const RUSTC_VERSION: &str;               // the rustc version, without the leading "rustc " prefix

// Rich — mode selection, profiles, Swagger, custom files, TTY colour.
pub enum BannerMode { Text, Minimal, Off }   // Spring Boot-style Banner.Mode
impl BannerMode { pub fn from_name(&str) -> Self; }  // case-insensitive, unknown -> Text

pub trait BannerConfig { fn get(&self, key: &str) -> Option<String>; }
// blanket impl for `Fn(&str) -> Option<String>` so a closure works too

pub struct BannerPrinter { /* … */ }
impl BannerPrinter {
    pub fn new() -> Self;                                  // Text mode, kernel + rustc versions
    pub fn from_config<C: BannerConfig>(&C) -> Self;       // firefly.banner.{mode,location}
    pub fn with_mode(self, BannerMode) -> Self;
    pub fn with_version / with_starter / with_app / with_app_version
          / with_rust_version (self, impl Into<String>) -> Self;
    pub fn with_profiles<I, S>(self, I) -> Self;           // I: IntoIterator<Item = S: Into<String>>
    pub fn with_swagger(self, impl Into<String>) -> Self;  // adds the SwaggerUI line
    pub fn with_location(self, impl Into<String>) -> Self; // custom banner file
    pub fn with_color(self, bool) -> Self;                 // force ANSI on/off
    pub fn render(&self) -> String;                        // always plain
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()>;  // plain unless forced
    pub fn print(&self) -> io::Result<()>;                 // stdout, colour when TTY
}
```

- **`Text`** — full art + metadata block (the default).
- **`Minimal`** — one line:
  `:: Firefly Framework for Rust :: (v..) app=.. profiles=..`.
- **`Off`** — renders nothing.

`from_config` reads `firefly.banner.mode` (`text` / `minimal` / `off`,
case-insensitive, unknown → `Text`) and `firefly.banner.location` (a custom
banner file; missing/unreadable falls back to the embedded template).
Observability stays decoupled from `firefly-config`: `from_config` takes
any `BannerConfig` (or a `Fn(&str) -> Option<String>` closure), so the caller
passes in the resolved `mode`/`location`.

**Colour.** `render()` and `write_to` are plain by default (so
captured/log output stays clean); `print()` colourises when stdout is a
terminal — red art, green foundation/license lines, bold tagline.
`with_color(true|false)` forces it either way.

The template lives in `crates/observability/banner.txt` (embedded via
`include_str!`); placeholders `{version}`, `{starter}`, `{app}`,
`{rust_version}`, `{profiles}` are substituted at render time. The compiler
version is captured by the build script from `rustc --version`. Called by
`firefly-starter-core` on startup.

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

## Metrics, tracing, and richer logging

Beyond the structured-logging, health, and banner essentials, the crate
provides a labeled-metrics registry, W3C trace-context propagation, process
metrics, per-target log levels, PII redaction, a rolling file appender, a
console/dev log renderer, and external logging-config-file loading.

### Labeled metrics + `timed()` / `counted()`

```rust,ignore
pub struct MetricsRegistry;
impl MetricsRegistry {
    pub fn new() -> Self;                    // process-global, idempotent
    pub fn isolated() -> Self;               // private registry (tests/exporters)
    pub fn counter(&self, name, desc, labels: &[&str]) -> Arc<Counter>;
    pub fn gauge(&self, name, desc, labels: &[&str]) -> Arc<Gauge>;
    pub fn histogram(&self, name, desc, labels: &[&str], buckets: Option<&[f64]>) -> Arc<Histogram>;
    pub fn prometheus_text(&self) -> String; // text exposition (counters as <name>_total)
}
// Counter/Gauge/Histogram: .labels(&["v", …]) -> Labeled* child series,
// inc/inc_by, set/add/inc/dec, observe; value()/value_with(), count()/sum().

pub async fn timed(®istry, name, fut) -> T;
pub async fn timed_result(®istry, name, fut) -> Result<T, E>;
pub async fn counted(®istry, name, fut) -> T;
pub async fn counted_result(®istry, name, fut) -> Result<T, E>;
pub struct Timed;   // builder: .description() .class() .method() .tag() .record()/.record_result()
pub struct Counted; // builder: same, counting result=success|failure + exception

// Metrics-recording PORT — write instrumentation against the
// abstraction, not the Prometheus adapter:
pub trait MetricsRecorder: Send + Sync {
    fn counter(&self, name, desc, labels: &[&str]) -> Arc<Counter>;
    fn gauge(&self, name, desc, labels: &[&str]) -> Arc<Gauge>;
    fn histogram(&self, name, desc, labels: &[&str], buckets: Option<&[f64]>) -> Arc<Histogram>;
}
impl MetricsRecorder for MetricsRegistry;    // the default (Prometheus) adapter
pub struct NoOpMetricsRecorder;              // discards everything
```

`MetricsRecorder` is the port instrumentation depends on so it is not
hard-coupled to Prometheus; `MetricsRegistry` is the default adapter and
`NoOpMetricsRecorder` is a dependency-free one for tests / metrics-disabled
deployments (so code can hold a recorder instead of guarding an `Option`). The
no-op recorder is backed by a private isolated registry whose data is never
exposed, yet still enforces the same label-arity contract — surfacing wiring
mistakes in tests.

Micrometer-style naming is used: `orders.process` → histogram
`orders_process_seconds` with `class`/`method`/`exception` labels;
counted meters are exposed as `<name>_total` with
`class`/`method`/`result`/`exception`. The `class`/`method` labels are
explicit builder fields. The `exception` label on `Err` is the unqualified
error type name (via `std::any::type_name`).

### W3C trace context

```rust,ignore
pub struct TraceParent { version, trace_id, parent_id, flags } // parse() / Display / sampled()
pub struct TraceState;                       // parse() / get() / entries() / Display
pub const TRACEPARENT_HEADER: &str;          // "traceparent"
pub const TRACESTATE_HEADER: &str;           // "tracestate"

pub async fn with_trace_context(tp: Option<String>, ts: Option<String>, fut) -> T;
pub fn current_traceparent() -> Option<String>;
pub fn current_tracestate() -> Option<String>;

pub struct TraceContextLayer;                // tower layer:
                                             //   parses inbound headers, stores TraceParent/
                                             //   TraceState in request extensions + task-locals
pub fn inject_headers(&mut http::HeaderMap); // outbound injection
pub fn inject_reqwest(reqwest::RequestBuilder) -> reqwest::RequestBuilder;
```

The crate implements the W3C trace-context wire format natively (lowercase
hex, version `ff` and all-zero ids rejected, future versions tolerated,
`tracestate` capped at 32 members). The kernel task-local carries the
correlation id; the trace-context pair lives in this crate's own tokio
task-locals. When a trace context is in scope, the logging layer also stamps
`trace_id`/`span_id` onto every record (see [Logging](#logging)).

### Process metrics

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

The meter names follow Micrometer/Spring Boot conventions, so standard
dashboards and alerts work against them out of the box.

### Per-target log levels + runtime `set_level`

```rust,ignore
pub struct LogConfig {
    // … existing fields unchanged …
    pub levels: BTreeMap<String, Level>,     // {root: INFO, "my.module": DEBUG}
    pub file: Option<FileConfig>,
    pub redaction: Option<RedactionConfig>,
}
impl LogConfig {
    pub fn with_target_level(self, target, Level) -> Self; // "root" routes to .level
    pub fn with_file(self, FileConfig) -> Self;
    pub fn with_redaction(self, RedactionConfig) -> Self;
}

pub struct LevelHandle;                      // runtime level control
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

### PII redaction

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
output is byte-identical unless redaction is opted in. Redaction is
regex-based and applies at the layer's writer boundary; the `PHONE`
patterns use a digit-boundary check in place of look-arounds (the `regex`
crate has no look-around support).

### Rolling file appender

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

### Console / dev log renderer

```rust,ignore
pub enum LogFormat { Json, Text, Console }   // +Console (json|logfmt|console)
impl LogConfig { pub fn with_console_colors(self, bool) -> Self; } // default false
```

`LogFormat::Console` renders a human-friendly `time [LEVEL] msg key=value`
line — leading `time`/`level`/`msg`, trailing fields. It is plain text by
default; `with_console_colors(true)` enables ANSI level coloring + dimmed
timestamp/field keys for an interactive terminal. Intended for local
development; the production JSON/logfmt wire shapes are unchanged.

### External logging-config-file loading

```rust,ignore
pub fn load_log_config(path, base: LogConfig) -> Result<LogConfig, ConfigLoadError>;
pub fn apply_external_config(path, base: LogConfig) -> (LogConfig, bool); // (merged, applied)
pub enum ConfigLoadError { NotFound, Io, Parse, UnsupportedFormat }
```

Load a logging definition from a file at startup and fold its `level` /
`format` / `service` / per-target `levels` over a base `LogConfig`. Two
shapes, chosen by extension — `.json` and `.properties` / `.conf` / `.ini`
(`key=value`, with per-target overrides under the `level.` prefix).
`apply_external_config` is deliberately lenient: an empty/missing path, read
error, parse error, or unsupported extension returns `(base, false)` — the
base config unchanged, startup never crashed. An unknown level/format value
is ignored, leaving that field untouched so a single bad value doesn't drop
the rest of the file. The schema is a small, language-neutral mapping onto
the `LogConfig` builder, enabling file-driven reconfiguration without a
recompile.

### Banner modes + colour

```rust,ignore
pub enum BannerMode { Text, Minimal, Off }
pub trait BannerConfig { fn get(&self, key: &str) -> Option<String>; }
pub struct BannerPrinter;
impl BannerPrinter {
    pub fn from_config<C: BannerConfig>(&C) -> Self; // firefly.banner.{mode,location}
    // with_mode/with_version/with_app/with_starter/with_app_version/
    // with_rust_version/with_profiles/with_swagger/with_location/with_color
    pub fn render(&self) -> String;           // plain
    pub fn write_to(&self, &mut impl Write) -> io::Result<()>;
    pub fn print(&self) -> io::Result<()>;    // stdout, ANSI when TTY
}
```

The richer banner surface offers a `BannerMode` (`Text` full art +
metadata, `Minimal` one line, `Off` nothing), a `from_config` that reads
`firefly.banner.mode` / `firefly.banner.location` through a decoupled
`BannerConfig` trait (or a `Fn(&str) -> Option<String>` closure), active
profiles, an optional Swagger-UI line, custom banner files, and TTY-aware
ANSI colour (red art, green foundation/license, bold tagline) with a plain
default for non-terminal/file writers.

## Testing

```bash
cargo test -p firefly-observability
```

Covers JSON-format correlation-id emission (sync and async task-local
scopes), the `degraded ⊕ up` overall computation, banner content and
overrides, level filtering, text format, span field merging, log↔trace
correlation (`trace_id`/`span_id` injected from the W3C trace context,
omitted with no context in scope), the JSON wire shape of `HealthResult`
(nanosecond `duration`, omitted empty `message`/`details`), the
`MetricsRecorder` port + `NoOpMetricsRecorder` adapter (recording, discard
semantics, trait-object use, label-arity enforcement), and Send/Sync bounds.

The metrics, tracing, and logging surface is covered by
`tests/pyfly_parity_test.rs`: metric idempotency across registries,
`@timed`/`@counted` Micrometer naming and tags, W3C inject/extract round
trips, the tracing-filter inbound-trace test, and the logging suite
(redaction engine/processor/patterns including Luhn, `parse_size`, rotation
+ backup pruning, per-logger levels and runtime `set_level`, the console
renderer, and `test_config_loader` for external-file reconfiguration).

The banner is covered by `tests/banner_parity_test.rs`: mode selection,
text/minimal/off rendering, custom file location, `from_config`, metadata
content, the metadata block, the optional Swagger-UI line, and TTY-aware
ANSI colour (forced on/off + plain non-TTY default).
