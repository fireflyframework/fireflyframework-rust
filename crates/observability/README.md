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

## Testing

```bash
cargo test -p firefly-observability
```

Covers JSON-format correlation-id emission (sync and async task-local
scopes), the `degraded ⊕ up` overall computation, banner content and
overrides, plus Rust-specific cases: level filtering, text format, span
field merging, the Go JSON wire shape of `HealthResult` (nanosecond
`duration`, omitted empty `message`/`details`), and Send/Sync bounds.
