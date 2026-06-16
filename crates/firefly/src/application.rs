// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! [`FireflyApplication`] — the turnkey bootstrap, the Rust analog of Spring
//! Boot's `SpringApplication.run(App.class, args)`.
//!
//! It does what every service used to hand-roll in a composition root: build the
//! web stack, **auto-register** the framework's infrastructure beans and
//! **component-scan** the application's beans into the DI container, drain the
//! discovered CQRS handlers / EDA listeners / `#[scheduled]` tasks, **auto-mount**
//! every `#[rest_controller]`, apply the middleware chain + security, self-host
//! the admin dashboard wired to the live components, start the scheduler, and
//! serve the public + management ports with graceful shutdown.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//! firefly::FireflyApplication::new("orders").version("1.0.0").run().await
//! # }
//! ```
//!
//! [`bootstrap`](FireflyApplication::bootstrap) returns the assembled (but not
//! yet served) [`Bootstrapped`] so tests can drive the public router in-process;
//! [`run`](FireflyApplication::run) bootstraps and serves.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use firefly_container::Container;
use firefly_cqrs::Bus;
use firefly_eda::Broker;
use firefly_scheduling::Scheduler;
use firefly_starter_core::InfoContributor;
use firefly_starter_web::{CoreConfig, WebStack};

/// The boxed error returned by the readiness hook and [`FireflyApplication::run`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ReadyFuture = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send>>;
type ReadyHook = Box<dyn FnOnce(AppContext) -> ReadyFuture + Send>;
type RoutesBuilder = Box<dyn FnOnce(&Arc<Container>) -> Router + Send>;

/// The live collaborators handed to a [`FireflyApplication::on_ready`] hook for
/// app-specific post-wiring (bus middleware, projection binding, …) after the
/// container is scanned but before the discovered handlers are drained and the
/// servers start.
pub struct AppContext {
    /// The scanned DI container (framework infra beans + app beans).
    pub container: Arc<Container>,
    /// The CQRS bus (add read-cache / custom middleware here).
    pub bus: Arc<Bus>,
    /// The event broker.
    pub broker: Arc<dyn Broker>,
    /// The task scheduler.
    pub scheduler: Arc<Scheduler>,
}

/// The turnkey application bootstrap — Spring Boot's `SpringApplication`.
pub struct FireflyApplication {
    config: CoreConfig,
    security: Option<(firefly_security::FilterChain, firefly_security::BearerLayer)>,
    on_ready: Option<ReadyHook>,
    extra_routes: Option<RoutesBuilder>,
    info_contributors: Vec<InfoContributor>,
    api_addr: String,
    management_addr: String,
}

/// The assembled-but-not-yet-served application returned by
/// [`FireflyApplication::bootstrap`].
pub struct Bootstrapped {
    /// The web stack (kept so [`serve`](Bootstrapped::serve) can run the lifecycle).
    pub web: WebStack,
    /// The scanned DI container.
    pub container: Arc<Container>,
    /// The fully-assembled public API router (controllers + middleware + security).
    pub api_router: Router,
    /// The management router (`/actuator/*` + the self-hosted `/admin` dashboard).
    pub management_router: Router,
    /// The task scheduler (started by [`serve`](Bootstrapped::serve)).
    pub scheduler: Arc<Scheduler>,
    /// The public bind address.
    pub api_addr: String,
    /// The management bind address.
    pub management_addr: String,
}

impl FireflyApplication {
    /// Starts a new application named `name`. Defaults the bind addresses from
    /// `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` (else `0.0.0.0:8080` /
    /// `0.0.0.0:8081`).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            config: CoreConfig {
                app_name: name.into(),
                ..Default::default()
            },
            security: None,
            on_ready: None,
            extra_routes: None,
            info_contributors: Vec::new(),
            api_addr: std::env::var("FIREFLY_SERVER_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            management_addr: std::env::var("FIREFLY_MANAGEMENT_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8081".to_string()),
        }
    }

    /// Sets the application version (surfaced on the banner + `/actuator/info`).
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.config.app_version = version.into();
        self
    }

    /// Tunes the [`CoreConfig`] (CORS, security headers, idempotency, …).
    #[must_use]
    pub fn configure(mut self, f: impl FnOnce(&mut CoreConfig)) -> Self {
        f(&mut self.config);
        self
    }

    /// Installs the security `FilterChain` (path-based RBAC) + the `BearerLayer`
    /// (token extraction) protecting the API.
    #[must_use]
    pub fn security(
        mut self,
        chain: firefly_security::FilterChain,
        bearer: firefly_security::BearerLayer,
    ) -> Self {
        self.security = Some((chain, bearer));
        self
    }

    /// Registers an app-specific readiness hook, run after the container is
    /// scanned (wire bus middleware, bind projections, …).
    #[must_use]
    pub fn on_ready<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce(AppContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.on_ready = Some(Box::new(move |ctx| Box::pin(hook(ctx))));
        self
    }

    /// Adds extra (non-`#[rest_controller]`) routes, built from the scanned
    /// container (e.g. a feature-gated streaming sub-router).
    #[must_use]
    pub fn extra_routes<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&Arc<Container>) -> Router + Send + 'static,
    {
        self.extra_routes = Some(Box::new(f));
        self
    }

    /// Adds an `/actuator/info` contributor.
    #[must_use]
    pub fn info_contributor(mut self, contributor: InfoContributor) -> Self {
        self.info_contributors.push(contributor);
        self
    }

    /// Overrides the public API bind address.
    #[must_use]
    pub fn api_addr(mut self, addr: impl Into<String>) -> Self {
        self.api_addr = addr.into();
        self
    }

    /// Overrides the management (actuator + admin) bind address.
    #[must_use]
    pub fn management_addr(mut self, addr: impl Into<String>) -> Self {
        self.management_addr = addr.into();
        self
    }

    /// Boots the application **without serving**: builds the web stack, scans the
    /// DI container, runs the readiness hook, drains discovered handlers /
    /// listeners / scheduled tasks, auto-mounts controllers, and assembles the
    /// public + management routers. Tests drive [`Bootstrapped::api_router`]
    /// in-process; [`run`](Self::run) calls this then serves.
    pub async fn bootstrap(self) -> Result<Bootstrapped, BoxError> {
        let FireflyApplication {
            config,
            security,
            on_ready,
            extra_routes,
            info_contributors,
            api_addr,
            management_addr,
        } = self;
        let app_name = config.app_name.clone();
        let app_version = config.app_version.clone();

        // 1. Web stack. Security is applied from a DI bean after the scan, so the
        //    stack is built plain + mutable here.
        let mut web = WebStack::new(config);

        // 2. Logging — tee into the admin capture buffer when the dashboard is on.
        #[cfg(feature = "admin")]
        let log_buffer = firefly_admin::LogBuffer::new();
        #[cfg(feature = "admin")]
        let _ = web.init_logging_with_layers(vec![Box::new(log_buffer.clone())]);
        #[cfg(not(feature = "admin"))]
        let _ = web.init_logging();

        // 3. DI: the framework auto-registers its infrastructure beans, then
        //    component-scan discovers + registers + autowires the app's beans.
        let container = Arc::new(Container::new());
        web.register_beans(&container);
        container.scan();
        // Await every `async fn` `#[bean]` factory (DB pools, broker dials, …)
        // now that the synchronous scan has registered every bean, so async
        // beans are live before the controllers / handlers / eager singletons
        // below resolve them. Fail-fast: a construction error aborts startup.
        container
            .init_async_beans()
            .await
            .map_err(|e| Box::new(e) as BoxError)?;

        let bus = Arc::clone(&web.bus);
        let broker = Arc::clone(&web.broker);
        let scheduler = web.scheduler();

        // 4. Auto-configure the CQRS bus: correlation propagation always; the
        //    read-cache middleware whenever a `QueryCache` bean is present
        //    (validation is already installed by `Core`).
        bus.use_middleware(firefly_cqrs::CorrelationMiddleware::new());
        if let Ok(query_cache) = container.resolve::<firefly_cqrs::QueryCache>() {
            bus.use_middleware(query_cache.middleware());
        }

        // 5. Optional app readiness hook (most apps need none — beans + the
        //    hooks below cover the wiring).
        if let Some(hook) = on_ready {
            hook(AppContext {
                container: Arc::clone(&container),
                bus: Arc::clone(&bus),
                broker: Arc::clone(&broker),
                scheduler: Arc::clone(&scheduler),
            })
            .await?;
        }

        // 6. Security: an explicitly-configured chain + bearer, else the
        //    `FilterChain` + `BearerLayer` DI beans (Spring's
        //    `SecurityFilterChain` bean). Captured here and layered onto the
        //    application routes ONLY in step 9 (not the public docs), so the
        //    auth boundary covers exactly the application surface while the
        //    observability edge still wraps everything.
        let (filter_chain, bearer) = match security {
            Some((chain, bearer)) => (Some(chain), Some(bearer)),
            None => {
                let chain = container
                    .resolve::<firefly_security::FilterChain>()
                    .ok()
                    .map(|chain| (*chain).clone());
                let bearer = container
                    .resolve::<firefly_security::BearerLayer>()
                    .ok()
                    .map(|bearer| (*bearer).clone());
                (chain, bearer)
            }
        };

        // 6b. Global exception advice (Spring's `@ControllerAdvice`): when an
        //     `ExceptionHandlerRegistry` bean is registered, install it as the
        //     outermost layer so every problem+json response is post-processed.
        if let Ok(registry) = container.resolve::<firefly_web::ExceptionHandlerRegistry>() {
            if !registry.is_empty() {
                web.set_exception_advice((*registry).clone());
            }
        }

        // 7. Public routes: auto-mounted `#[rest_controller]`s + every
        //    `RouteContributor` bean (+ any explicit extra routes). Resolving the
        //    controllers here constructs their autowired collaborators (e.g. the
        //    `@Bean` ledger, which seeds the event-sourcing projection) before the
        //    listeners are drained.
        let mut routes = firefly_web::mount_controllers(&container)
            .merge(firefly_web::mount_route_contributors(&container));
        if let Some(extra) = extra_routes {
            routes = routes.merge(extra(&container));
        }

        // 8. Drain the discovered handlers / listeners / scheduled tasks — both
        //    the free-`fn` registrations and the **bean** registrations
        //    (`#[handlers]` methods that autowire their collaborators from the
        //    container, resolved here exactly like a Spring handler component).
        firefly_cqrs::register_discovered_handlers(&bus);
        firefly_cqrs::register_discovered_handler_beans(&bus, &container);
        firefly_eda::subscribe_discovered_listeners(broker.as_ref()).await?;
        firefly_eda::subscribe_discovered_listener_beans(broker.as_ref(), &container).await?;
        firefly_scheduling::register_discovered_scheduled(&scheduler);
        firefly_scheduling::register_discovered_scheduled_beans(&scheduler, &container);

        // 9. Assemble the public router and wrap EVERY request in the
        //    observability edge. Security (the filter chain + bearer) is layered
        //    onto the application routes ONLY; the default 404 is a public sibling
        //    (the OpenAPI docs live on the management surface — step 7).
        //    `web.apply_middleware` then wraps the whole router in the inherited
        //    edge — idempotency, the request-access-log, metrics, correlation, W3C
        //    trace, security headers, problem rendering, CORS, and the global
        //    exception advice — so application requests and unmatched-route 404s
        //    are logged, traced, and correlated (no observability gap).
        let mut app = routes;
        if let Some(chain) = filter_chain {
            app = app.layer(chain.layer());
        }
        if let Some(bearer) = bearer {
            app = app.layer(bearer);
        }

        // 9b. OpenAPI: the spec is built from the live inventory — every
        //     `#[rest_controller]` route plus every `#[derive(Schema)]` DTO. The
        //     docs router (`/v3/api-docs`, `/swagger-ui`, `/redoc`) is built here
        //     but mounted on the **management** surface (step 7), not the public
        //     API: Swagger UI / ReDoc / the spec expose the whole API surface and
        //     every schema — a control-plane concern that belongs beside the
        //     actuator + admin dashboard, so the public data-plane port never
        //     serves them.
        let openapi = firefly_openapi::Builder::new(firefly_openapi::Info {
            title: format!("{app_name} API"),
            version: if app_version.is_empty() {
                "0.0.0".to_string()
            } else {
                app_version.clone()
            },
            ..firefly_openapi::Info::default()
        })
        // The docs are served on the management port, but the API is on the
        // public port — so the spec advertises the API base URL as its `server`,
        // and Swagger UI / ReDoc "Try it out" send requests *there*, not to the
        // management origin they're loaded from. Override with
        // `FIREFLY_OPENAPI_SERVER_URL` (e.g. a public URL behind a reverse proxy).
        .add_server(firefly_openapi::Server {
            url: api_server_url(&api_addr),
            description: "API server".to_string(),
        })
        .from_inventory();
        let docs_router = openapi.docs_router(&firefly_openapi::DocsConfig::default());

        // 9c. Default 404: an unmatched route answers a proper RFC 9457
        //     `application/problem+json` instead of axum's bare empty body (which
        //     a browser offers to download as a blank file). The fallback is set
        //     on the public router so the edge below logs + traces it too.
        let combined = app.fallback(not_found_fallback);

        let api = web.apply_middleware(combined);

        // 9d. Admin trace capture at the outermost edge so EVERY request — app,
        //     docs, and 404 — appears in the dashboard's Traces view. (Shadowed
        //     under the feature so the binding stays immutable without `admin`.)
        #[cfg(feature = "admin")]
        let trace_buffer = Arc::new(firefly_admin::TraceBuffer::new());
        #[cfg(feature = "admin")]
        let api = api.layer(firefly_admin::TraceLayer::new(Arc::clone(&trace_buffer)));

        // 7. Management router: actuator + the self-hosted admin dashboard,
        //    wired to the live components (health, metrics, bus, scheduler,
        //    container, env, traces, logs).
        let management = web.actuator_router(info_contributors);
        #[cfg(feature = "admin")]
        let management = {
            let deps = firefly_admin::AdminDeps {
                scheduler: Some(Arc::clone(&scheduler)),
                bus: Some(Arc::clone(&bus)),
                container: Some(Arc::clone(&container)),
                environment: Some(env_snapshot(&app_name, &app_version)),
                ..firefly_admin::AdminDeps::new(
                    app_name.clone(),
                    app_version.clone(),
                    web.health_composite(),
                    web.metric_registry(),
                    trace_buffer,
                    log_buffer,
                )
            };
            management.merge(firefly_admin::mount(
                firefly_admin::AdminConfig::default(),
                deps,
            ))
        };
        // The OpenAPI docs (Swagger UI / ReDoc / spec) live on the management
        // surface, beside the actuator + admin dashboard — never on the public
        // API port. Their paths (`/swagger-ui`, `/redoc`, `/v3/api-docs`) do not
        // collide with `/actuator/*` or `/admin/`, and the UIs load the spec
        // same-origin from this port.
        // An unknown path on the management surface answers the same RFC 9457
        // `application/problem+json` 404 the public API does, not axum's bare
        // empty body. (Safe: neither the actuator nor the admin router sets a
        // fallback, so this is the single fallback for the merged router.)
        let management = management.merge(docs_router).fallback(not_found_fallback);

        Ok(Bootstrapped {
            web,
            container,
            api_router: api,
            management_router: management,
            scheduler,
            api_addr,
            management_addr,
        })
    }

    /// Boots and serves the application: the public API on [`api_addr`], the
    /// management surface (actuator + admin) on [`management_addr`], the
    /// scheduler running, and graceful SIGINT/SIGTERM shutdown. Returns when the
    /// application stops cleanly.
    ///
    /// [`api_addr`]: Self::api_addr
    /// [`management_addr`]: Self::management_addr
    pub async fn run(self) -> Result<(), BoxError> {
        self.bootstrap().await?.serve().await
    }
}

impl Bootstrapped {
    /// Starts the scheduler and serves the assembled routers with graceful
    /// shutdown, blocking until the application stops.
    pub async fn serve(self) -> Result<(), BoxError> {
        let Bootstrapped {
            web,
            container,
            api_router,
            management_router,
            scheduler,
            api_addr,
            management_addr,
        } = self;

        // Run the scheduled tasks on a background task.
        let scheduler_runner = Arc::clone(&scheduler);
        tokio::spawn(async move { scheduler_runner.start().await });

        web.print_banner();
        #[cfg(feature = "admin")]
        println!(":: admin dashboard :: http://{management_addr}/admin/");
        println!(
            ":: api docs (management) :: swagger-ui http://{management_addr}/swagger-ui | redoc http://{management_addr}/redoc | spec http://{management_addr}/v3/api-docs"
        );
        log_startup_report(&container);

        let application = web
            .new_application()
            .on_server("api", move |shutdown| async move {
                let listener = tokio::net::TcpListener::bind(&api_addr).await?;
                axum::serve(listener, api_router)
                    .with_graceful_shutdown(shutdown.wait())
                    .await?;
                Ok(())
            })
            .on_server("management", move |shutdown| async move {
                let listener = tokio::net::TcpListener::bind(&management_addr).await?;
                axum::serve(listener, management_router)
                    .with_graceful_shutdown(shutdown.wait())
                    .await?;
                Ok(())
            });

        match application.run().await {
            Ok(()) => Ok(()),
            // A handle/signal-triggered stop is a clean shutdown.
            Err(err) if err.is_cancelled() => Ok(()),
            Err(err) => Err(Box::new(err)),
        }
    }
}

/// The default fallback for unmatched routes: a proper RFC 9457
/// `application/problem+json` **404**, rendered exactly like every other
/// framework error (same `type`/`title`/`status` envelope, same
/// `application/problem+json` content type) — so an unknown path returns a
/// readable problem document instead of axum's bare empty body, which a browser
/// offers to download as a blank file.
async fn not_found_fallback(
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> axum::response::Response {
    let detail = format!("No route matches {method} {}", uri.path());
    firefly_web::problem_response(&firefly_kernel::ProblemDetail::not_found(detail))
}

/// The client-reachable base URL of the **public API**, for the OpenAPI spec's
/// `servers` entry. The docs are served on the management port, so this is what
/// makes Swagger UI / ReDoc "Try it out" call the API port instead of the docs
/// origin. `FIREFLY_OPENAPI_SERVER_URL` overrides it (e.g. a public URL behind a
/// reverse proxy); otherwise it is derived from the API bind address — a
/// wildcard host (`0.0.0.0` / `[::]`) is not client-usable, so it falls back to
/// `localhost`.
fn api_server_url(api_addr: &str) -> String {
    if let Ok(url) = std::env::var("FIREFLY_OPENAPI_SERVER_URL") {
        if !url.trim().is_empty() {
            return url.trim().to_string();
        }
    }
    match api_addr.parse::<std::net::SocketAddr>() {
        Ok(addr) => {
            let host = if addr.ip().is_unspecified() {
                "localhost".to_string()
            } else {
                match addr.ip() {
                    std::net::IpAddr::V6(v6) => format!("[{v6}]"),
                    std::net::IpAddr::V4(v4) => v4.to_string(),
                }
            };
            format!("http://{host}:{}", addr.port())
        }
        // Not a bare socket address (e.g. already a host:port name) — best effort.
        Err(_) => format!("http://{api_addr}"),
    }
}

/// Emits the pyfly/Spring-Boot-style **line-by-line startup report** — the
/// active profiles, every discovered/registered bean (grouped by stereotype),
/// the auto-mounted route table, and the handler/listener/scheduled counts — so
/// a boot log reads like Spring Boot's console: you can see exactly what the
/// framework wired.
fn log_startup_report(container: &Container) {
    let short = |s: &str| s.rsplit("::").next().unwrap_or(s).to_string();

    let profiles = firefly_config::active_profiles("default");
    let profiles = if profiles.is_empty() {
        "default".to_string()
    } else {
        profiles.join(", ")
    };
    println!(":: active profiles :: {profiles}");

    let mut beans = container.beans();
    beans.sort_by(|a, b| {
        a.stereotype
            .cmp(&b.stereotype)
            .then_with(|| a.name.cmp(&b.name))
    });
    println!(":: beans ({}) ::", beans.len());
    for bean in &beans {
        let stereotype = bean.stereotype.as_deref().unwrap_or("bean");
        println!(
            "     [{:<13}] {:<22} {:<10} ({})",
            stereotype,
            short(&bean.name),
            bean.scope,
            short(&bean.type_name),
        );
    }

    let mut routes: Vec<_> = firefly_container::routes().collect();
    routes.sort_by_key(|r| (r.path, r.method));
    println!(":: routes ({}) ::", routes.len());
    for route in &routes {
        println!(
            "     {:<6} {:<32} -> {}::{}",
            route.method, route.path, route.controller, route.handler,
        );
    }

    println!(
        ":: cqrs handlers: {} | event listeners: {} | scheduled tasks: {} | controllers: {} ::",
        firefly_cqrs::discovered_handler_count() + firefly_cqrs::discovered_handler_bean_count(),
        firefly_eda::discovered_listener_count() + firefly_eda::discovered_listener_bean_count(),
        firefly_scheduling::discovered_scheduled_count()
            + firefly_scheduling::discovered_scheduled_bean_count(),
        firefly_web::controller_count(),
    );

    let schema_count = firefly_container::schemas().count();
    println!(
        ":: openapi :: {} operations | {} component schemas (served at /v3/api-docs) ::",
        routes.len(),
        schema_count,
    );
}

/// Builds the admin dashboard's environment snapshot from the app identity, the
/// active profiles, and the `FIREFLY_*` process environment.
#[cfg(feature = "admin")]
fn env_snapshot(name: &str, version: &str) -> firefly_admin::EnvironmentSnapshot {
    use std::collections::BTreeMap;

    use firefly_admin::{EnvironmentSnapshot, PropertyEntry, PropertySource};

    let entry = |value: &str| PropertyEntry {
        value: value.to_string(),
        origin: "firefly".to_string(),
    };
    let app_props = BTreeMap::from([
        ("firefly.application.name".to_string(), entry(name)),
        ("firefly.application.version".to_string(), entry(version)),
    ]);

    let mut env_props = BTreeMap::new();
    for (key, value) in std::env::vars() {
        if key.starts_with("FIREFLY_") {
            env_props.insert(
                key,
                PropertyEntry {
                    value,
                    origin: "System Environment Property".to_string(),
                },
            );
        }
    }

    let mut sources = Vec::new();
    if !env_props.is_empty() {
        sources.push(PropertySource {
            name: "systemEnvironment".to_string(),
            properties: env_props,
        });
    }
    sources.push(PropertySource {
        name: "firefly".to_string(),
        properties: app_props,
    });

    EnvironmentSnapshot::new(firefly_config::active_profiles("default"), sources)
}
