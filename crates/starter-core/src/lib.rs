//! # firefly-starter-core
//!
//! The **one-call infrastructure-tier wiring** for any Firefly Rust
//! service — the port of the Go `startercore` module (Java original:
//! `firefly-starter-core`, .NET: `FireflyFramework.Starter.Core`).
//!
//! A single [`Core::new`] returns a [`Core`] struct holding every
//! component a typical service needs:
//!
//! * `log` — [`LogConfig`] with the service name pre-set (install it
//!   with [`Core::init_logging`]; every record then carries the
//!   correlation id, exactly like the Go `slog.Logger` default).
//! * `cache` — [`Adapter`], default [`MemoryAdapter`].
//! * `bus` — [`Bus`] with the [`ValidationMiddleware`] pre-installed.
//! * `broker` — [`Broker`], default [`InMemoryBroker`].
//! * `health` — [`HealthComposite`] with a default cache health
//!   indicator.
//! * `idempotency` — [`IdempotencyConfig`].
//! * `metrics` — [`MetricRegistry`].
//! * `scheduler` — [`Scheduler`].
//!
//! Plus the convenience methods mirroring the Go `Core`:
//!
//! * [`Core::apply_middleware`] — the canonical outermost HTTP
//!   middleware chain (problem renderer, correlation, idempotency) —
//!   Go's `Middleware()`.
//! * [`Core::actuator_router`] — pre-wired
//!   `/actuator/{health,info,metrics,env,tasks,version}` router —
//!   Go's `ActuatorHandler(infoContributors...)`.
//! * [`Core::new_application`] — [`Application`] named after the app —
//!   Go's `NewApplication()`.
//! * [`Core::print_banner`] — emits the ASCII banner identifying
//!   starter + app + runtime.
//!
//! ## pyfly-parity batteries (all OFF by default)
//!
//! Mirroring how pyfly's starters/auto-configuration assemble the stack,
//! [`CoreConfig`] carries `Option`-typed knobs that — when set — weave
//! the P1 middleware surfaces into [`Core::apply_middleware`] /
//! [`Core::actuator_router`] at their canonical pyfly filter order:
//!
//! | Knob | Effect when `Some` |
//! |------|--------------------|
//! | [`cors`](CoreConfig::cors) | [`CorsLayer`] at the outermost edge (preflight + simple-request decoration) |
//! | [`security_headers`](CoreConfig::security_headers) | [`SecurityHeadersLayer`] (OWASP response headers) |
//! | [`csrf`](CoreConfig::csrf) | [`CsrfLayer`] (double-submit cookie) |
//! | [`request_log`](CoreConfig::request_log) | [`RequestLogLayer`] (one structured access-log event per request) |
//! | [`request_metrics`](CoreConfig::request_metrics) | [`MetricsLayer`] bridged into the actuator [`MetricRegistry`] via [`MetricRegistryObserver`] |
//! | [`http_exchanges`](CoreConfig::http_exchanges) | [`HttpExchangesLayer`] recording + `/actuator/httpexchanges` |
//! | [`loggers`](CoreConfig::loggers) | `/actuator/loggers[/{name}]` runtime log-level control |
//! | [`redaction`](CoreConfig::redaction) | PII scrubbing on the default log writer |
//!
//! Leaving every knob unset (the default) reproduces exactly the
//! Go-parity Problem → Correlation → Idempotency chain and actuator
//! surface — so existing wire shapes and tests are byte-for-byte
//! unchanged. The web brief notes [`RequestObserver`] is a *local* trait
//! in `firefly-web` (which does not depend on `firefly-actuator`);
//! [`MetricRegistryObserver`] is the bridge, and this is the crate that
//! depends on both, so it lives here.
//!
//! ### Building a downstream admin dashboard
//!
//! A downstream `firefly-admin` `AdminDeps` is assembled from the public
//! `Core` accessors — [`Core::cqrs_bus`], [`Core::scheduler`],
//! [`Core::health_composite`], [`Core::metric_registry`],
//! [`Core::http_exchanges`], [`Core::loggers`]. `firefly-starter-core`
//! does **not** depend on `firefly-admin` (that crate is a separate,
//! later-tier dependency), so no `Core::admin_deps()` convenience is
//! offered here — wiring it would invert the dependency graph. The
//! accessors are all public; the admin crate constructs its `AdminDeps`
//! from a `&Core` (or shared `Arc<Core>`) instead.
//!
//! ## Health glue
//!
//! The Go module stores an `observability.Composite` and hands it to
//! `actuator.Mount`. In this port the actuator crate carries its own
//! health primitives ([`HealthComposite`] / [`HealthIndicator`]), so
//! [`Core::new`] wires that type directly — and the
//! [`ObservabilityIndicator`] bridge (plus
//! [`Core::add_observability_indicator`]) lets any
//! [`firefly_observability::Indicator`] feed the `/actuator/health`
//! endpoint exactly as in Go. Both sides emit the identical JSON wire
//! shape (`status`, `message`, `details`, `duration` in nanoseconds,
//! `time`).
//!
//! ## Quick start
//!
//! ```no_run
//! use axum::{routing::get, Router};
//! use firefly_starter_core::{Core, CoreConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let core = Core::new(CoreConfig {
//!         app_name: "orders".into(),
//!         app_version: "1.0.0".into(),
//!         ..CoreConfig::default()
//!     });
//!     core.init_logging()?;
//!     core.print_banner();
//!
//!     let api = core.apply_middleware(
//!         Router::new().route("/orders", get(|| async { "[]" })),
//!     );
//!     let admin = core.actuator_router(Vec::new());
//!
//!     let app = core
//!         .new_application()
//!         .on_server("api", move |shutdown| async move {
//!             let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
//!             axum::serve(listener, api)
//!                 .with_graceful_shutdown(shutdown.wait())
//!                 .await?;
//!             Ok(())
//!         })
//!         .on_server("admin", move |shutdown| async move {
//!             let listener = tokio::net::TcpListener::bind("0.0.0.0:8081").await?;
//!             axum::serve(listener, admin)
//!                 .with_graceful_shutdown(shutdown.wait())
//!                 .await?;
//!             Ok(())
//!         });
//!     app.run().await?;
//!     Ok(())
//! }
//! ```

#![warn(missing_docs)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;

pub use firefly_actuator::{
    ActuatorConfig, HealthComposite, HealthIndicator, HealthResult, HealthStatus,
    HttpExchangeRecorder, HttpExchangesLayer, IndicatorFn, InfoContributor, LoggersState,
    MetricRegistry,
};
pub use firefly_cache::{Adapter, MemoryAdapter};
pub use firefly_cqrs::{Bus, ValidationMiddleware};
pub use firefly_eda::{Broker, InMemoryBroker};
pub use firefly_lifecycle::{Application, ShutdownHandle, ShutdownSignal};
pub use firefly_observability::{Indicator, LogConfig, LogFormat, RedactionConfig};
pub use firefly_scheduling::Scheduler;
pub use firefly_web::{
    CorrelationLayer, CorsConfig, CorsLayer, CsrfLayer, IdempotencyConfig, IdempotencyLayer,
    MetricsLayer, Outcome, ProblemLayer, RequestLogLayer, RequestMetric, RequestObserver,
    SecurityHeadersConfig, SecurityHeadersLayer, HTTP_SERVER_REQUESTS_MAX_METRIC,
    HTTP_SERVER_REQUESTS_METRIC,
};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = firefly_kernel::VERSION;

/// Tunes [`Core::new`]. All fields are optional; unset values fall back
/// to the canonical defaults documented in `CONFIGURATION.md` — the Rust
/// spelling of the Go `startercore.Config` struct.
#[derive(Default)]
pub struct CoreConfig {
    /// Application name; defaults to `"firefly-app"` when empty.
    pub app_name: String,
    /// Application version; surfaced on `/actuator/info` and
    /// `/actuator/version` (the actuator substitutes the framework
    /// version when empty).
    pub app_version: String,
    /// Active starter name; defaults to `"starter-core"` when empty.
    pub starter_name: String,
    /// Logging configuration; defaults to JSON at info level with
    /// `service` set to the app name — Go's
    /// `observability.NewLogger(LogConfig{Service: AppName, Format: "json"})`.
    pub log: Option<LogConfig>,
    /// Cache adapter; defaults to [`MemoryAdapter`] — Go's `cache.NewMemory()`.
    pub cache: Option<Arc<dyn Adapter>>,
    /// CQRS bus; defaults to a fresh [`Bus`] — Go's `cqrs.New()`.
    pub bus: Option<Arc<Bus>>,
    /// Event broker; defaults to [`InMemoryBroker`] — Go's `eda.NewInMemory()`.
    pub broker: Option<Arc<dyn Broker>>,
    /// Health composite; defaults to an empty [`HealthComposite`].
    pub health: Option<Arc<HealthComposite>>,
    /// Idempotency middleware config; defaults to
    /// [`IdempotencyConfig::default`] (memory store, 24 h TTL,
    /// POST/PUT/PATCH) — Go's `web.DefaultIdempotencyConfig()`.
    pub idempotency: Option<IdempotencyConfig>,
    /// Metric registry; defaults to an empty [`MetricRegistry`] —
    /// Go's `actuator.NewRegistry()`.
    pub metrics: Option<Arc<MetricRegistry>>,
    /// Task scheduler; defaults to an empty [`Scheduler`] — Go's
    /// `scheduling.New()`.
    pub scheduler: Option<Arc<Scheduler>>,

    // ---- pyfly-parity optional middleware (all OFF by default) ----------
    /// When `Some`, [`Core::apply_middleware`] adds the
    /// [`CorsLayer`](firefly_web::CorsLayer) at the outermost edge of the
    /// chain — the Rust spelling of pyfly's `pyfly.web.cors.*` /
    /// Starlette `CORSMiddleware`. `None` (default) leaves CORS off, so
    /// existing wire shapes are unchanged.
    pub cors: Option<CorsConfig>,

    /// When `Some`, [`Core::apply_middleware`] adds the
    /// [`SecurityHeadersLayer`](firefly_web::SecurityHeadersLayer) — the
    /// OWASP response-header set from pyfly's `pyfly.web.security_headers`.
    /// `None` (default) leaves the headers off.
    pub security_headers: Option<SecurityHeadersConfig>,

    /// When `Some`, [`Core::apply_middleware`] adds the
    /// [`CsrfLayer`](firefly_web::CsrfLayer) (double-submit cookie) — the
    /// Rust spelling of pyfly's `CsrfFilter`. Pass
    /// `Some(CsrfLayer::new())` for the default exclude set, or a
    /// customized layer. `None` (default) leaves CSRF off.
    pub csrf: Option<CsrfLayer>,

    /// When `Some`, [`Core::apply_middleware`] adds the
    /// [`RequestLogLayer`](firefly_web::RequestLogLayer) (one structured
    /// access-log `tracing` event per request) — pyfly's
    /// `RequestLoggingFilter`. `None` (default) leaves it off.
    pub request_log: Option<RequestLogLayer>,

    /// When `Some`, [`Core::apply_middleware`] adds the Micrometer
    /// [`MetricsLayer`](firefly_web::MetricsLayer), bridging web's local
    /// [`RequestObserver`](firefly_web::RequestObserver) into this core's
    /// actuator [`MetricRegistry`] (the labeled
    /// `http_server_requests_seconds` timer + `…_max` gauge). This is the
    /// crate that depends on both web and actuator, so the bridge lives
    /// here. `None` (default) leaves request metrics off.
    pub request_metrics: Option<RequestMetricsConfig>,

    /// When `Some`, [`Core::apply_middleware`] records each exchange into
    /// this shared [`HttpExchangeRecorder`], and [`Core::actuator_router`]
    /// serves it on `/actuator/httpexchanges` — pyfly's
    /// `HttpExchangeRecorderFilter` + endpoint. `None` (default) leaves
    /// recording off and the endpoint unmounted.
    pub http_exchanges: Option<Arc<HttpExchangeRecorder>>,

    /// When `Some`, [`Core::actuator_router`] mounts
    /// `GET/POST /actuator/loggers[/{name}]` over the wrapped runtime
    /// log-level state — pyfly's `LoggersEndpoint`. Build it from a
    /// `tracing_subscriber` reload handle (see [`LoggersState`]). `None`
    /// (default) leaves the endpoint unmounted.
    pub loggers: Option<Arc<LoggersState>>,

    /// When `Some`, the default [`LogConfig`] is built with this PII
    /// [`RedactionConfig`] installed on the log writer — pyfly's
    /// `pyfly.logging.redaction.*`. Ignored when an explicit `log` config
    /// is supplied (which then owns its own redaction). `None` (default)
    /// leaves redaction off.
    pub redaction: Option<RedactionConfig>,
}

/// Tunes the HTTP server-metrics bridge wired by
/// [`Core::apply_middleware`] when [`CoreConfig::request_metrics`] is set
/// — the Rust spelling of pyfly's `MetricsFilter` knobs.
#[derive(Debug, Clone, Default)]
pub struct RequestMetricsConfig {
    /// Rolling-max window step in seconds; `None` keeps Micrometer's
    /// 60-second default.
    pub step_seconds: Option<f64>,
    /// Path globs excluded from instrumentation; `None` keeps pyfly's
    /// defaults (`/actuator/prometheus`, `/admin/api/sse/*`).
    pub exclude_patterns: Option<Vec<String>>,
}

/// The bag of wired components a service receives from [`Core::new`] —
/// the Rust spelling of the Go `startercore.Core` struct.
pub struct Core {
    /// Application name (`"firefly-app"` default).
    pub app_name: String,
    /// Application version as configured (may be empty).
    pub app_version: String,
    /// Active starter name (`"starter-core"` default).
    pub starter_name: String,
    /// Logging configuration; install globally with [`Core::init_logging`].
    pub log: LogConfig,
    /// The wired cache adapter.
    pub cache: Arc<dyn Adapter>,
    /// The wired CQRS bus, with [`ValidationMiddleware`] installed.
    pub bus: Arc<Bus>,
    /// The wired event broker.
    pub broker: Arc<dyn Broker>,
    /// The wired health composite, including the default `cache`
    /// indicator.
    pub health: Arc<HealthComposite>,
    /// The idempotency middleware configuration applied by
    /// [`Core::apply_middleware`].
    pub idempotency: IdempotencyConfig,
    /// The wired metric registry, exposed on `/actuator/metrics`.
    pub metrics: Arc<MetricRegistry>,
    /// The wired task scheduler.
    pub scheduler: Arc<Scheduler>,

    // ---- pyfly-parity optional middleware (None unless configured) ------
    /// The CORS layer applied by [`Core::apply_middleware`], when
    /// [`CoreConfig::cors`] was set.
    cors: Option<CorsLayer>,
    /// The security-headers layer applied by [`Core::apply_middleware`],
    /// when [`CoreConfig::security_headers`] was set.
    security_headers: Option<SecurityHeadersLayer>,
    /// The CSRF layer applied by [`Core::apply_middleware`], when
    /// [`CoreConfig::csrf`] was set.
    csrf: Option<CsrfLayer>,
    /// The request-log layer applied by [`Core::apply_middleware`], when
    /// [`CoreConfig::request_log`] was set.
    request_log: Option<RequestLogLayer>,
    /// The HTTP server-metrics layer applied by
    /// [`Core::apply_middleware`], bridging into [`Core::metrics`]; set
    /// when [`CoreConfig::request_metrics`] was configured.
    request_metrics: Option<MetricsLayer>,
    /// The shared HTTP-exchange recorder fed by [`Core::apply_middleware`]
    /// and served by [`Core::actuator_router`], when
    /// [`CoreConfig::http_exchanges`] was set. Read it with
    /// [`Core::http_exchanges`].
    http_exchanges: Option<Arc<HttpExchangeRecorder>>,
    /// The runtime log-level state served by [`Core::actuator_router`] on
    /// `/actuator/loggers`, when [`CoreConfig::loggers`] was set.
    loggers: Option<Arc<LoggersState>>,
}

impl Core {
    /// Wires the core infrastructure with the given config — Go's
    /// `startercore.New(cfg)`.
    ///
    /// Defaults applied (matching the Go module field-for-field):
    ///
    /// * empty `app_name` → `"firefly-app"`,
    /// * empty `starter_name` → `"starter-core"`,
    /// * `log` → JSON / info with `service` = app name,
    /// * `cache` → [`MemoryAdapter`], `bus` → [`Bus::new`],
    ///   `broker` → [`InMemoryBroker`], `health` → empty composite,
    ///   `metrics` → empty registry, `scheduler` → [`Scheduler::new`],
    ///   `idempotency` → [`IdempotencyConfig::default`].
    ///
    /// Two pieces of wiring happen unconditionally, exactly like Go:
    ///
    /// 1. [`ValidationMiddleware`] is installed on the bus, so every
    ///    dispatched message's [`validate`](firefly_cqrs::Message::validate)
    ///    hook is honoured out of the box.
    /// 2. A `cache` health indicator is added to the composite, so
    ///    `/actuator/health` surfaces cache reachability out of the box.
    pub fn new(cfg: CoreConfig) -> Self {
        let app_name = if cfg.app_name.is_empty() {
            "firefly-app".to_string()
        } else {
            cfg.app_name
        };
        let starter_name = if cfg.starter_name.is_empty() {
            "starter-core".to_string()
        } else {
            cfg.starter_name
        };
        let log = cfg.log.unwrap_or_else(|| {
            let base = LogConfig::new().with_service(app_name.clone());
            match cfg.redaction {
                Some(redaction) => base.with_redaction(redaction),
                None => base,
            }
        });
        let cache = cfg.cache.unwrap_or_else(|| Arc::new(MemoryAdapter::new()));
        let bus = cfg.bus.unwrap_or_else(|| Arc::new(Bus::new()));
        let broker = cfg
            .broker
            .unwrap_or_else(|| Arc::new(InMemoryBroker::new()));
        let health = cfg
            .health
            .unwrap_or_else(|| Arc::new(HealthComposite::new()));
        let metrics = cfg
            .metrics
            .unwrap_or_else(|| Arc::new(MetricRegistry::new()));
        let scheduler = cfg.scheduler.unwrap_or_else(|| Arc::new(Scheduler::new()));
        let idempotency = cfg.idempotency.unwrap_or_default();

        // Wire validation middleware on the bus by default.
        bus.use_middleware(ValidationMiddleware::new());

        // Wire a default cache health indicator so /actuator/health
        // surfaces cache reachability out of the box.
        let probe_cache = Arc::clone(&cache);
        health.add(IndicatorFn::new("cache", move || {
            let cache = Arc::clone(&probe_cache);
            async move {
                match cache.health_check().await {
                    Ok(()) => HealthResult::up(),
                    Err(err) => HealthResult::down(err.to_string()),
                }
            }
        }));

        // Build the optional pyfly-parity middleware. Each stays `None`
        // unless the matching `CoreConfig` knob was set, so the default
        // wire shape (and every existing test) is unchanged.
        let cors = cfg.cors.map(CorsLayer::new);
        let security_headers = cfg.security_headers.map(SecurityHeadersLayer::new);
        let csrf = cfg.csrf;
        let request_log = cfg.request_log;
        let request_metrics = cfg.request_metrics.map(|rm| {
            // Bridge web's RequestObserver into this core's actuator
            // MetricRegistry — the canonical place, since this crate is
            // the one that depends on both web and actuator.
            let observer: Arc<dyn RequestObserver> =
                Arc::new(MetricRegistryObserver::new(Arc::clone(&metrics)));
            let mut layer = MetricsLayer::new(observer);
            if let Some(step) = rm.step_seconds {
                layer = layer.with_step(step);
            }
            if let Some(patterns) = rm.exclude_patterns {
                layer = layer.with_exclude_patterns(patterns);
            }
            layer
        });
        let http_exchanges = cfg.http_exchanges;
        let loggers = cfg.loggers;

        Core {
            app_name,
            app_version: cfg.app_version,
            starter_name,
            log,
            cache,
            bus,
            broker,
            health,
            idempotency,
            metrics,
            scheduler,
            cors,
            security_headers,
            csrf,
            request_log,
            request_metrics,
            http_exchanges,
            loggers,
        }
    }

    /// Wraps `router` in the canonical outermost middleware chain:
    /// panic-recovering Problem renderer, correlation id, idempotency —
    /// Go's `Middleware()`. Apply at the outermost layer, after every
    /// route has been added.
    ///
    /// When the pyfly-parity knobs on [`CoreConfig`] are set, the matching
    /// layers are woven into the chain at their canonical pyfly order
    /// (lower order = nearer the network edge). The effective
    /// outermost → innermost order (matching pyfly's filter order, lower
    /// order = nearer the network edge) is:
    ///
    /// ```text
    /// CorsLayer            (cors)              — Starlette CORSMiddleware edge
    /// ProblemLayer         (always)            — panic → 500 RFC7807
    /// SecurityHeadersLayer (security_headers)  — decorate every response
    /// CorrelationLayer     (always)            — X-Correlation-Id (+ ctx)
    /// MetricsLayer         (request_metrics)   — http_server_requests_* (order -100)
    /// HttpExchangesLayer   (http_exchanges)    — record into the recorder (order -90)
    /// RequestLogLayer      (request_log)       — one access-log event (order +200)
    /// CsrfLayer            (csrf)              — double-submit cookie (order +210)
    /// IdempotencyLayer     (always)            — replay on Idempotency-Key (order +230)
    ///         │
    ///         ▼
    ///      your router
    /// ```
    ///
    /// Every optional layer defaults to OFF, so a [`Core::new`] built from
    /// a bare [`CoreConfig`] produces exactly the Go-parity
    /// Problem → Correlation → Idempotency chain (unchanged wire shape).
    pub fn apply_middleware(&self, router: Router) -> Router {
        // axum's `Router::layer` makes the *last* applied layer the
        // outermost, so build the stack inner → outer, in the reverse of
        // the table above.
        let mut router = router;

        // Idempotency is innermost (just above the handler), then CSRF,
        // request-log, http-exchanges, metrics — pyfly's +230 → -100.
        router = router.layer(IdempotencyLayer::new(self.idempotency.clone()));
        if let Some(csrf) = &self.csrf {
            router = router.layer(csrf.clone());
        }
        if let Some(request_log) = &self.request_log {
            router = router.layer(*request_log);
        }
        if let Some(recorder) = &self.http_exchanges {
            router = router.layer(HttpExchangesLayer::new(Arc::clone(recorder)));
        }
        if let Some(metrics) = &self.request_metrics {
            router = router.layer(metrics.clone());
        }

        // Correlation (always) wraps the request-scoped layers above so
        // every access-log / metric / exchange sees the correlation id.
        router = router.layer(CorrelationLayer::new());

        // SecurityHeaders sits just outside Correlation (so it decorates
        // every response) but inside Problem (so it also decorates the
        // recovered 500 body).
        if let Some(security_headers) = &self.security_headers {
            router = router.layer(security_headers.clone());
        }

        // Problem renderer recovers panics from every inner layer.
        router = router.layer(ProblemLayer::new());

        // CORS is the outermost edge, wrapping even the Problem renderer,
        // exactly like Starlette's CORSMiddleware in pyfly.
        if let Some(cors) = &self.cors {
            router = router.layer(cors.clone());
        }

        router
    }

    /// Returns the `/actuator/*` router bound to this core's health
    /// composite and metric registry — Go's
    /// `ActuatorHandler(infoContributors...)`. Serve it from a dedicated
    /// admin port so the management surface never leaks onto the public
    /// network.
    ///
    /// When [`CoreConfig::loggers`] / [`CoreConfig::http_exchanges`] were
    /// set, the matching endpoints (`/actuator/loggers[/{name}]`,
    /// `/actuator/httpexchanges`) are mounted too — otherwise they stay
    /// off, preserving the default surface.
    pub fn actuator_router(&self, info_contributors: Vec<InfoContributor>) -> Router {
        firefly_actuator::mount(ActuatorConfig {
            app_name: self.app_name.clone(),
            app_version: self.app_version.clone(),
            health: Arc::clone(&self.health),
            metric_registry: Arc::clone(&self.metrics),
            info_contributors,
            loggers: self.loggers.clone(),
            http_exchanges: self.http_exchanges.clone(),
            ..ActuatorConfig::default()
        })
    }

    /// The shared HTTP-exchange recorder, when
    /// [`CoreConfig::http_exchanges`] was set — the accessor a downstream
    /// admin dashboard (e.g. `firefly-admin`'s `AdminDeps`) reads to
    /// serve `/admin/api/httpexchanges`. `None` when recording is off.
    pub fn http_exchanges(&self) -> Option<Arc<HttpExchangeRecorder>> {
        self.http_exchanges.clone()
    }

    /// The runtime log-level state, when [`CoreConfig::loggers`] was set.
    /// `None` when the loggers endpoint is off.
    pub fn loggers(&self) -> Option<Arc<LoggersState>> {
        self.loggers.clone()
    }

    /// The wired CQRS bus — a convenience accessor mirroring the public
    /// [`Core::bus`] field, so a downstream admin dashboard can be built
    /// from a shared reference to the core without moving it.
    pub fn cqrs_bus(&self) -> Arc<Bus> {
        Arc::clone(&self.bus)
    }

    /// The wired task scheduler — a convenience accessor mirroring the
    /// public [`Core::scheduler`] field (the admin dashboard's
    /// scheduled-tasks view reads it).
    pub fn scheduler(&self) -> Arc<Scheduler> {
        Arc::clone(&self.scheduler)
    }

    /// The wired health composite — a convenience accessor mirroring the
    /// public [`Core::health`] field (the admin dashboard's health view
    /// reads it).
    pub fn health_composite(&self) -> Arc<HealthComposite> {
        Arc::clone(&self.health)
    }

    /// The wired metric registry — a convenience accessor mirroring the
    /// public [`Core::metrics`] field.
    pub fn metric_registry(&self) -> Arc<MetricRegistry> {
        Arc::clone(&self.metrics)
    }

    /// Returns a [`Application`] named after the app — Go's
    /// `NewApplication()`. Service authors append start/stop hooks and
    /// servers, then call `.run().await`. (Where the Go application
    /// carried the core's `slog.Logger`, the Rust lifecycle crate logs
    /// through the global `tracing` subscriber — install this core's
    /// config with [`Core::init_logging`] first.)
    pub fn new_application(&self) -> Application {
        Application::new(self.app_name.clone())
    }

    /// Installs this core's [`LogConfig`] as the global `tracing`
    /// subscriber — the Rust counterpart of the Go core building its
    /// default `observability.NewLogger`. Fails if a global subscriber
    /// was already installed.
    pub fn init_logging(&self) -> Result<(), tracing::subscriber::SetGlobalDefaultError> {
        firefly_observability::init_logging(self.log.clone())
    }

    /// Registers an observability [`Indicator`] on this core's actuator
    /// health composite via the [`ObservabilityIndicator`] bridge — the
    /// Rust spelling of Go's `core.Health.Add(observability.IndicatorFunc{…})`.
    pub fn add_observability_indicator<I>(&self, indicator: I)
    where
        I: Indicator + 'static,
    {
        self.health.add(ObservabilityIndicator::new(indicator));
    }

    /// Renders the startup banner for this starter + app as a `String` —
    /// useful for tests and custom writers.
    pub fn banner(&self) -> String {
        firefly_observability::banner_string(&self.starter_name, &self.app_name)
    }

    /// Emits the startup banner to stdout — Go's `PrintBanner()`. Write
    /// errors are ignored, like Go's discarded `fmt.Fprintf` result.
    pub fn print_banner(&self) {
        let _ = firefly_observability::print_banner(
            &mut std::io::stdout(),
            &self.starter_name,
            &self.app_name,
        );
    }
}

/// Bridges a [`firefly_observability::Indicator`] onto the actuator's
/// [`HealthIndicator`] trait so observability probes feed
/// `/actuator/health` exactly as in Go (where the actuator consumes the
/// observability composite directly). Status, message, details,
/// duration, and timestamp all carry over — both crates emit the same
/// JSON wire shape, so the bridge is lossless.
pub struct ObservabilityIndicator {
    inner: Arc<dyn Indicator>,
}

impl ObservabilityIndicator {
    /// Wraps an owned observability indicator.
    pub fn new<I: Indicator + 'static>(indicator: I) -> Self {
        Self::from_arc(Arc::new(indicator))
    }

    /// Wraps an already-shared observability indicator.
    pub fn from_arc(indicator: Arc<dyn Indicator>) -> Self {
        Self { inner: indicator }
    }
}

#[async_trait]
impl HealthIndicator for ObservabilityIndicator {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn check(&self) -> HealthResult {
        to_actuator_result(self.inner.check().await)
    }
}

/// Converts an observability [`firefly_observability::Status`] into the
/// actuator's [`HealthStatus`]. The four wire names (`UP`, `DOWN`,
/// `DEGRADED`, `UNKNOWN`) map one-to-one.
pub fn to_actuator_status(status: firefly_observability::Status) -> HealthStatus {
    match status {
        firefly_observability::Status::Up => HealthStatus::Up,
        firefly_observability::Status::Down => HealthStatus::Down,
        firefly_observability::Status::Degraded => HealthStatus::Degraded,
        firefly_observability::Status::Unknown => HealthStatus::Unknown,
    }
}

/// Bridges `firefly-web`'s local [`RequestObserver`] trait onto the
/// actuator [`MetricRegistry`], so the HTTP server-metrics middleware
/// feeds the same labeled `http_server_requests_seconds` timer (and
/// companion `…_max` gauge) the rest of the framework, Spring Boot, and
/// every Grafana dashboard expect.
///
/// `firefly-web` deliberately keeps [`RequestObserver`] a *local* trait
/// (it does not depend on `firefly-actuator`); `firefly-starter-core` is
/// the crate that depends on both, so the bridge lives here — the Rust
/// spelling of pyfly's `MetricsFilter` writing into the Prometheus
/// registry. Each observation records the request duration into the
/// histogram and sets the rolling-max gauge, both tagged with the five
/// Micrometer labels (`method`, `uri`, `status`, `outcome`,
/// `exception`); a request that did not panic carries `exception="None"`,
/// matching pyfly's sentinel.
pub struct MetricRegistryObserver {
    registry: Arc<MetricRegistry>,
}

impl MetricRegistryObserver {
    /// Wraps the registry that observations are recorded into.
    pub fn new(registry: Arc<MetricRegistry>) -> Self {
        Self { registry }
    }
}

impl RequestObserver for MetricRegistryObserver {
    fn record(&self, metric: &RequestMetric) {
        let status = metric.status.to_string();
        let outcome = metric.outcome.as_str();
        // pyfly tags a clean request with the sentinel "None"; only a
        // panicking handler carries a real exception label.
        let exception = metric.exception.as_deref().unwrap_or("None");
        let labels: [(&str, &str); 5] = [
            ("method", &metric.method),
            ("uri", &metric.uri),
            ("status", &status),
            ("outcome", outcome),
            ("exception", exception),
        ];
        self.registry
            .histogram_with(HTTP_SERVER_REQUESTS_METRIC, &labels)
            .observe(metric.duration_seconds);
        self.registry
            .gauge_with(HTTP_SERVER_REQUESTS_MAX_METRIC, &labels)
            .set(metric.rolling_max_seconds);
    }
}

/// Converts an observability [`firefly_observability::HealthResult`]
/// into the actuator's [`HealthResult`]. Empty details map to `None`,
/// matching both sides' `omitempty` JSON encoding.
pub fn to_actuator_result(result: firefly_observability::HealthResult) -> HealthResult {
    let details = if result.details.is_empty() {
        None
    } else {
        Some(result.details.into_iter().collect())
    };
    HealthResult {
        status: to_actuator_status(result.status),
        message: result.message,
        details,
        duration: result.duration,
        time: result.time,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use firefly_cache::CacheError;
    use firefly_cqrs::{CqrsError, Message};
    use firefly_eda::{handler, Event};
    use firefly_kernel::{HEADER_CORRELATION_ID, PROBLEM_CONTENT_TYPE};
    use http_body_util::BodyExt;
    use serde::Serialize;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    fn core_for(app_name: &str) -> Core {
        Core::new(CoreConfig {
            app_name: app_name.into(),
            ..CoreConfig::default()
        })
    }

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[derive(Clone, Serialize)]
    struct CreateOrder {
        id: String,
    }

    impl Message for CreateOrder {
        fn validate(&self) -> Result<(), CqrsError> {
            if self.id.is_empty() {
                return Err(CqrsError::validation("id required"));
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct OrderCreated {
        id: String,
    }

    /// A cache adapter whose backend is permanently unreachable.
    struct FailingCache;

    #[async_trait]
    impl Adapter for FailingCache {
        async fn get(&self, _key: &str) -> Result<Vec<u8>, CacheError> {
            Err(CacheError::Backend("redis gone".into()))
        }

        async fn set(
            &self,
            _key: &str,
            _value: &[u8],
            _ttl: Option<Duration>,
        ) -> Result<(), CacheError> {
            Err(CacheError::Backend("redis gone".into()))
        }

        async fn delete(&self, _key: &str) -> Result<(), CacheError> {
            Err(CacheError::Backend("redis gone".into()))
        }

        async fn clear(&self) -> Result<(), CacheError> {
            Err(CacheError::Backend("redis gone".into()))
        }

        fn name(&self) -> String {
            "failing".into()
        }

        async fn health_check(&self) -> Result<(), CacheError> {
            Err(CacheError::Backend("redis gone".into()))
        }
    }

    // ---- ports of the Go test suite ----------------------------------------

    /// Go: TestCoreDefaults.
    #[tokio::test]
    async fn core_defaults() {
        let c = core_for("orders");
        assert_eq!(c.app_name, "orders");
        assert_eq!(c.starter_name, "starter-core");
        assert_eq!(c.cache.name(), "memory");
        assert_eq!(c.log.service, "orders");
        assert_eq!(c.log.format, LogFormat::Json);
        // The default cache health indicator is wired in.
        let (overall, results) = c.health.check_all().await;
        assert_eq!(overall, HealthStatus::Up);
        assert_eq!(results["cache"].status, HealthStatus::Up);
        // Bus and broker are live defaults.
        c.bus.register(|cmd: CreateOrder| async move {
            Ok::<_, CqrsError>(OrderCreated { id: cmd.id })
        });
        let created: OrderCreated = c.bus.send(CreateOrder { id: "o1".into() }).await.unwrap();
        assert_eq!(created.id, "o1");
        c.broker
            .publish(Event::new("noop", "Noop", "test", None))
            .await
            .unwrap();
    }

    /// Go: TestCoreDefaults (empty-name fallbacks).
    #[test]
    fn core_defaults_fall_back_to_canonical_names() {
        let c = Core::new(CoreConfig::default());
        assert_eq!(c.app_name, "firefly-app");
        assert_eq!(c.starter_name, "starter-core");
        assert_eq!(c.log.service, "firefly-app");
        assert!(c.app_version.is_empty());
    }

    /// Go: TestCoreMiddlewareChainHandlesError.
    #[tokio::test]
    async fn core_middleware_chain_handles_error() {
        async fn boom() -> &'static str {
            panic!("boom")
        }

        let c = core_for("orders");
        let app = c.apply_middleware(Router::new().route("/x", get(boom)));

        let res = app
            .oneshot(Request::get("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            PROBLEM_CONTENT_TYPE
        );
    }

    /// Go: TestCoreBannerNonEmpty — made real: the Go test was a stub
    /// because it could not re-route stdout; here the banner is rendered
    /// to a string.
    #[test]
    fn core_banner_non_empty() {
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            starter_name: "starter-core".into(),
            ..CoreConfig::default()
        });
        let banner = c.banner();
        assert!(!banner.is_empty());
        assert!(banner.contains("Firefly Framework for Rust"));
        assert!(banner.contains("orders"));
        assert!(banner.contains("starter-core"));
    }

    // ---- boot sequence ------------------------------------------------------

    /// The canonical boot flow: wire the core, mount the public router
    /// behind the middleware chain and the actuator on the admin side,
    /// probe /actuator/health, dispatch a command, publish an event,
    /// then stop the lifecycle application via its handle.
    #[tokio::test]
    async fn boot_mounts_routers_dispatches_and_shuts_down() {
        let core = Core::new(CoreConfig {
            app_name: "orders".into(),
            app_version: "1.0.0".into(),
            ..CoreConfig::default()
        });

        // Public router behind the canonical middleware chain.
        let api = core.apply_middleware(Router::new().route(
            "/orders",
            post(|| async { (StatusCode::CREATED, "order-1") }),
        ));
        // Admin router with the actuator surface.
        let admin = core.actuator_router(Vec::new());

        // 1. /actuator/health reports UP with the default cache probe.
        let res = admin
            .clone()
            .oneshot(
                Request::get("/actuator/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let health = body_json(res).await;
        assert_eq!(health["status"], "UP");
        assert_eq!(health["details"]["cache"]["status"], "UP");

        // 2. The public route responds through the middleware chain and
        //    carries a correlation id.
        let res = api
            .clone()
            .oneshot(Request::post("/orders").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        assert!(res.headers().contains_key(HEADER_CORRELATION_ID));

        // 3. Dispatch a command through the pre-wired bus.
        core.bus.register(|cmd: CreateOrder| async move {
            Ok::<_, CqrsError>(OrderCreated { id: cmd.id })
        });
        let created: OrderCreated = core
            .bus
            .send(CreateOrder { id: "o-42".into() })
            .await
            .unwrap();
        assert_eq!(created, OrderCreated { id: "o-42".into() });

        // 4. Publish an event through the pre-wired broker.
        let deliveries = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&deliveries);
        core.broker
            .subscribe(
                "orders.created",
                handler(move |ev: Event| {
                    let seen = Arc::clone(&seen);
                    async move {
                        assert_eq!(ev.event_type, "OrderCreated");
                        seen.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            )
            .await
            .unwrap();
        core.broker
            .publish(Event::new(
                "orders.created",
                "OrderCreated",
                "orders",
                Some(br#"{"id":"o-42"}"#.to_vec()),
            ))
            .await
            .unwrap();
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);

        // 5. Run the lifecycle application and stop it via the handle.
        let app = core
            .new_application()
            .with_drain_timeout(Duration::from_millis(500))
            .on_server("api", move |shutdown| async move {
                shutdown.wait().await;
                Ok(())
            });
        let handle = app.shutdown_handle();
        handle.shutdown();
        let err = app.run().await.expect_err("handle stop reports Cancelled");
        assert!(err.is_cancelled(), "run: {err}");
    }

    // ---- Rust-specific coverage ---------------------------------------------

    /// New() installs the validation middleware unconditionally, like
    /// Go's `cfg.Bus.Use(cqrs.ValidationMiddleware())`.
    #[tokio::test]
    async fn validation_middleware_wired_by_default() {
        let c = core_for("orders");
        c.bus.register(|cmd: CreateOrder| async move {
            Ok::<_, CqrsError>(OrderCreated { id: cmd.id })
        });
        let err = c
            .bus
            .send::<CreateOrder, OrderCreated>(CreateOrder { id: String::new() })
            .await
            .expect_err("invalid command must be rejected");
        assert!(matches!(err, CqrsError::Validation(_)));
        assert_eq!(err.to_string(), "id required");
    }

    /// The default cache indicator turns a failing backend into a 503
    /// DOWN on /actuator/health, with the adapter error as the message.
    #[tokio::test]
    async fn cache_health_indicator_reports_down() {
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            cache: Some(Arc::new(FailingCache)),
            ..CoreConfig::default()
        });
        let admin = c.actuator_router(Vec::new());
        let res = admin
            .oneshot(
                Request::get("/actuator/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let health = body_json(res).await;
        assert_eq!(health["status"], "DOWN");
        assert_eq!(health["details"]["cache"]["status"], "DOWN");
        assert_eq!(
            health["details"]["cache"]["message"],
            "firefly/cache: backend: redis gone"
        );
    }

    /// Observability indicators feed /actuator/health via the bridge,
    /// preserving status, message, and structured details.
    #[tokio::test]
    async fn observability_indicator_feeds_actuator_health() {
        let c = core_for("orders");
        c.add_observability_indicator(firefly_observability::IndicatorFn::new("queue", || async {
            firefly_observability::HealthResult::degraded("cold start").with_detail("depth", 3)
        }));

        let admin = c.actuator_router(Vec::new());
        let res = admin
            .oneshot(
                Request::get("/actuator/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // DEGRADED still answers 200, like every other port.
        assert_eq!(res.status(), StatusCode::OK);
        let health = body_json(res).await;
        assert_eq!(health["status"], "DEGRADED");
        assert_eq!(health["details"]["queue"]["status"], "DEGRADED");
        assert_eq!(health["details"]["queue"]["message"], "cold start");
        assert_eq!(health["details"]["queue"]["details"]["depth"], 3);
        assert_eq!(health["details"]["cache"]["status"], "UP");
    }

    #[test]
    fn status_conversion_maps_one_to_one() {
        use firefly_observability::Status;
        assert_eq!(to_actuator_status(Status::Up), HealthStatus::Up);
        assert_eq!(to_actuator_status(Status::Down), HealthStatus::Down);
        assert_eq!(to_actuator_status(Status::Degraded), HealthStatus::Degraded);
        assert_eq!(to_actuator_status(Status::Unknown), HealthStatus::Unknown);
    }

    #[test]
    fn result_conversion_is_lossless_and_omits_empty_details() {
        let plain = to_actuator_result(firefly_observability::HealthResult::up());
        assert_eq!(plain.status, HealthStatus::Up);
        assert!(plain.message.is_empty());
        assert!(plain.details.is_none());

        let rich = to_actuator_result(
            firefly_observability::HealthResult::down("dead").with_detail("attempts", 2),
        );
        assert_eq!(rich.status, HealthStatus::Down);
        assert_eq!(rich.message, "dead");
        let details = rich.details.expect("details preserved");
        assert_eq!(details["attempts"], 2);
    }

    /// The wired idempotency middleware replays the first 2xx response
    /// for a repeated Idempotency-Key, marking it Idempotent-Replay.
    #[tokio::test]
    async fn idempotency_replay_through_core_middleware() {
        let hits = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&hits);
        let c = core_for("orders");
        let app = c.apply_middleware(Router::new().route(
            "/orders",
            post(move || {
                let counter = Arc::clone(&counter);
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    (StatusCode::CREATED, format!("order-{n}")).into_response()
                }
            }),
        ));

        let request = || {
            Request::post("/orders")
                .header("Idempotency-Key", "k1")
                .body(Body::from(r#"{"sku":"a"}"#))
                .unwrap()
        };

        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        assert!(first.headers().get("Idempotent-Replay").is_none());
        let first_body = first.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&first_body[..], b"order-1");

        let second = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(second.status(), StatusCode::CREATED);
        assert_eq!(second.headers().get("Idempotent-Replay").unwrap(), "true");
        let second_body = second.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&second_body[..], b"order-1");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "handler ran exactly once");
    }

    /// The correlation layer echoes an incoming X-Correlation-Id.
    #[tokio::test]
    async fn correlation_id_echoed_through_core_middleware() {
        let c = core_for("orders");
        let app = c.apply_middleware(Router::new().route("/ping", get(|| async { "pong" })));
        let res = app
            .oneshot(
                Request::get("/ping")
                    .header(HEADER_CORRELATION_ID, "abc-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.headers().get(HEADER_CORRELATION_ID).unwrap(), "abc-123");
    }

    /// /actuator/version reflects the configured app name and version.
    #[tokio::test]
    async fn actuator_version_reflects_app() {
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            app_version: "1.0.0".into(),
            ..CoreConfig::default()
        });
        let admin = c.actuator_router(Vec::new());
        let res = admin
            .oneshot(
                Request::get("/actuator/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let version = body_json(res).await;
        assert_eq!(version["firefly"], VERSION);
        assert_eq!(version["app"], "orders");
        assert_eq!(version["appVersion"], "1.0.0");
    }

    /// Info contributors flow through actuator_router, like Go's
    /// variadic infoContributors.
    #[tokio::test]
    async fn actuator_info_contributors_merged() {
        let c = core_for("orders");
        let contributor: InfoContributor = Box::new(|| {
            let mut m = serde_json::Map::new();
            m.insert("git".into(), serde_json::json!({ "sha": "abc123" }));
            m
        });
        let admin = c.actuator_router(vec![contributor]);
        let res = admin
            .oneshot(Request::get("/actuator/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let info = body_json(res).await;
        assert_eq!(info["app"]["name"], "orders");
        assert_eq!(info["git"]["sha"], "abc123");
    }

    #[test]
    fn new_application_carries_app_name() {
        let c = core_for("orders");
        let app = c.new_application();
        assert_eq!(app.name(), "orders");
    }

    #[test]
    fn version_matches_kernel() {
        assert_eq!(VERSION, firefly_kernel::VERSION);
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn core_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Core>();
        assert_send_sync::<ObservabilityIndicator>();
        assert_send_sync::<MetricRegistryObserver>();
    }

    // ---- pyfly-parity middleware wiring -------------------------------------

    /// Every optional knob is OFF by default, so a bare `CoreConfig`
    /// produces no `cors` / `security_headers` / `csrf` / `request_log` /
    /// `request_metrics` / `http_exchanges` / `loggers` wiring — the
    /// invariant that keeps every existing wire shape and test unchanged.
    #[test]
    fn optional_middleware_off_by_default() {
        let c = core_for("orders");
        assert!(c.cors.is_none());
        assert!(c.security_headers.is_none());
        assert!(c.csrf.is_none());
        assert!(c.request_log.is_none());
        assert!(c.request_metrics.is_none());
        assert!(c.http_exchanges.is_none());
        assert!(c.loggers.is_none());
        assert!(c.http_exchanges().is_none());
        assert!(c.loggers().is_none());
    }

    /// With nothing configured, `apply_middleware` still emits the
    /// Go-parity chain: a plain GET carries a correlation id and no CORS /
    /// security headers leak onto the response.
    #[tokio::test]
    async fn default_chain_unchanged_when_knobs_off() {
        let c = core_for("orders");
        let app = c.apply_middleware(Router::new().route("/ping", get(|| async { "pong" })));
        let res = app
            .oneshot(Request::get("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().contains_key(HEADER_CORRELATION_ID));
        // No optional middleware ran.
        assert!(res.headers().get("x-frame-options").is_none());
        assert!(res.headers().get("access-control-allow-origin").is_none());
    }

    /// The headline boot test: CORS preflight is short-circuited, security
    /// headers decorate the response, AND the request-metrics counter
    /// increments — all through `Core::apply_middleware`.
    #[tokio::test]
    async fn boot_cors_preflight_security_headers_and_metrics_counter() {
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            cors: Some(CorsConfig {
                allowed_origins: vec!["https://app.example".into()],
                allowed_methods: vec!["GET".into(), "POST".into()],
                allowed_headers: vec!["content-type".into()],
                ..CorsConfig::default()
            }),
            security_headers: Some(SecurityHeadersConfig::default()),
            request_metrics: Some(RequestMetricsConfig::default()),
            ..CoreConfig::default()
        });

        let app = c.apply_middleware(
            Router::new().route("/orders/:id", get(|| async { (StatusCode::OK, "order") })),
        );

        // 1. CORS preflight is short-circuited with the allow-* set.
        let preflight = app
            .clone()
            .oneshot(
                Request::options("/orders/1")
                    .header("Origin", "https://app.example")
                    .header("Access-Control-Request-Method", "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preflight.status(), StatusCode::OK);
        assert_eq!(
            preflight
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "https://app.example"
        );

        // A disallowed origin is rejected at the edge.
        let blocked = app
            .clone()
            .oneshot(
                Request::options("/orders/1")
                    .header("Origin", "https://evil.example")
                    .header("Access-Control-Request-Method", "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::BAD_REQUEST);

        // 2. A real GET carries the security headers + the CORS origin.
        let res = app
            .clone()
            .oneshot(
                Request::get("/orders/42")
                    .header("Origin", "https://app.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            res.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(
            res.headers().get("access-control-allow-origin").unwrap(),
            "https://app.example"
        );

        // 3. The metrics bridge recorded the request into the core's
        //    registry under the templated uri and SUCCESS outcome.
        let detail = c
            .metrics
            .meter_json(HTTP_SERVER_REQUESTS_METRIC, None)
            .expect("http_server_requests_seconds is registered");
        let count = detail["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["statistic"] == "COUNT")
            .unwrap()["value"]
            .as_f64()
            .unwrap();
        assert!(count >= 1.0, "at least the GET was counted: {count}");
        let prom = c.metrics.render();
        assert!(
            prom.contains("uri=\"/orders/{id}\""),
            "templated uri tag present: {prom}"
        );
        assert!(
            prom.contains("outcome=\"SUCCESS\""),
            "success outcome tag present: {prom}"
        );
        assert!(
            prom.contains("exception=\"None\""),
            "clean request carries the None sentinel: {prom}"
        );
        // The companion rolling-max gauge is published too.
        assert!(
            prom.contains(HTTP_SERVER_REQUESTS_MAX_METRIC),
            "max gauge present: {prom}"
        );
    }

    /// The metrics bridge tags a panicking handler with `exception="panic"`
    /// and a 500 status, while the Problem renderer still recovers it.
    #[tokio::test]
    async fn metrics_bridge_records_panic_as_500() {
        async fn boom() -> &'static str {
            panic!("kaboom")
        }
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            request_metrics: Some(RequestMetricsConfig::default()),
            ..CoreConfig::default()
        });
        let app = c.apply_middleware(Router::new().route("/x", get(boom)));
        let res = app
            .oneshot(Request::get("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        // Problem renderer recovered the panic.
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let prom = c.metrics.render();
        assert!(prom.contains("exception=\"panic\""), "{prom}");
        assert!(prom.contains("status=\"500\""), "{prom}");
    }

    /// When `http_exchanges` is wired, `apply_middleware` records each
    /// request and `actuator_router` serves it on `/actuator/httpexchanges`.
    #[tokio::test]
    async fn http_exchanges_recorded_and_served() {
        let recorder = Arc::new(HttpExchangeRecorder::new());
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            http_exchanges: Some(Arc::clone(&recorder)),
            ..CoreConfig::default()
        });
        assert!(Arc::ptr_eq(&c.http_exchanges().unwrap(), &recorder));

        let api = c.apply_middleware(Router::new().route("/orders", get(|| async { "[]" })));
        let _ = api
            .oneshot(Request::get("/orders").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(recorder.len(), 1, "the request was recorded");

        let admin = c.actuator_router(Vec::new());
        let res = admin
            .oneshot(
                Request::get("/actuator/httpexchanges")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["exchanges"][0]["request"]["uri"], "/orders");
    }

    /// When `loggers` is wired, `actuator_router` mounts
    /// `/actuator/loggers`; otherwise the endpoint stays 404.
    #[tokio::test]
    async fn loggers_endpoint_mounted_when_wired() {
        // Off by default → 404.
        let bare = core_for("orders").actuator_router(Vec::new());
        let res = bare
            .oneshot(
                Request::get("/actuator/loggers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // Wired → 200 with Spring's level vocabulary.
        let loggers = Arc::new(LoggersState::with_reload_fn(|_| Ok(()), "info"));
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            loggers: Some(Arc::clone(&loggers)),
            ..CoreConfig::default()
        });
        assert!(Arc::ptr_eq(&c.loggers().unwrap(), &loggers));
        let admin = c.actuator_router(Vec::new());
        let res = admin
            .oneshot(
                Request::get("/actuator/loggers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["loggers"]["ROOT"]["configuredLevel"], "INFO");
    }

    /// The CSRF layer, when wired, rejects an unsafe request that lacks the
    /// double-submit token pair with a 403.
    #[tokio::test]
    async fn csrf_layer_guards_unsafe_requests_when_wired() {
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            csrf: Some(CsrfLayer::new()),
            ..CoreConfig::default()
        });
        let app = c.apply_middleware(
            Router::new().route("/orders", post(|| async { (StatusCode::CREATED, "ok") })),
        );
        let res = app
            .oneshot(Request::post("/orders").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    /// The request-log layer, when wired, does not change the response and
    /// the chain still produces a correlation id.
    #[tokio::test]
    async fn request_log_layer_is_transparent_when_wired() {
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            request_log: Some(RequestLogLayer::new()),
            ..CoreConfig::default()
        });
        let app = c.apply_middleware(Router::new().route("/ping", get(|| async { "pong" })));
        let res = app
            .oneshot(Request::get("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().contains_key(HEADER_CORRELATION_ID));
    }

    /// Idempotency replay still works with the full optional stack wired,
    /// proving the layer ordering keeps idempotency innermost.
    #[tokio::test]
    async fn idempotency_replay_survives_full_optional_stack() {
        let hits = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&hits);
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            cors: Some(CorsConfig::permit_defaults()),
            security_headers: Some(SecurityHeadersConfig::default()),
            request_log: Some(RequestLogLayer::new()),
            request_metrics: Some(RequestMetricsConfig::default()),
            http_exchanges: Some(Arc::new(HttpExchangeRecorder::new())),
            ..CoreConfig::default()
        });
        let app = c.apply_middleware(Router::new().route(
            "/orders",
            post(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::CREATED, "order").into_response()
                }
            }),
        ));
        let req = || {
            Request::post("/orders")
                .header("Idempotency-Key", "k1")
                .body(Body::from("{}"))
                .unwrap()
        };
        let first = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        // Drain the first body so the idempotency record is persisted
        // (the capture body stores the record when its last frame is
        // polled) before the replay request arrives.
        let _ = first.into_body().collect().await.unwrap().to_bytes();
        let second = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(second.headers().get("Idempotent-Replay").unwrap(), "true");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "handler ran exactly once");
    }

    /// The redaction knob installs PII scrubbing on the default log writer
    /// (only when no explicit `log` config is supplied).
    #[test]
    fn redaction_knob_installs_on_default_log() {
        let c = Core::new(CoreConfig {
            app_name: "orders".into(),
            redaction: Some(RedactionConfig::default()),
            ..CoreConfig::default()
        });
        // The log config carries the redaction settings through to the
        // writer; service name is still the app name.
        assert_eq!(c.log.service, "orders");
        assert!(c.log.redaction.is_some(), "redaction wired onto LogConfig");
    }

    /// The RequestObserver bridge records straight into a registry,
    /// independent of the HTTP layer — the unit boundary of the bridge.
    #[test]
    fn metric_registry_observer_records_timer_and_max() {
        let registry = Arc::new(MetricRegistry::new());
        let observer = MetricRegistryObserver::new(Arc::clone(&registry));
        observer.record(&RequestMetric {
            method: "GET".into(),
            uri: "/orders/{id}".into(),
            status: 200,
            outcome: Outcome::Success,
            exception: None,
            duration_seconds: 0.012,
            rolling_max_seconds: 0.012,
        });
        let prom = registry.render();
        assert!(
            prom.contains("http_server_requests_seconds_count"),
            "{prom}"
        );
        assert!(prom.contains("exception=\"None\""), "{prom}");
        assert!(
            prom.contains("http_server_requests_seconds_max{"),
            "max gauge labeled: {prom}"
        );
    }
}
