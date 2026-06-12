# Production & Deployment

This chapter covers everything between "it works on my machine" and a service
running reliably in production: the lifecycle and graceful shutdown, the
management surface, TLS, the optional hardening middleware, container packaging,
and a deployment checklist.

## The lifecycle and graceful shutdown

`firefly-lifecycle`'s `Application` is the Rust analog of
`SpringApplication.run()`: it traps SIGINT/SIGTERM, runs each server task with
its own drain signal, and grants a drain budget (default 30 s) before exiting.
`Core::new_application()` gives you one named after your app.

```rust,no_run
use firefly_starter_core::{Core, CoreConfig};

# async fn ex() -> Result<(), Box<dyn std::error::Error>> {
let core = Core::new(CoreConfig { app_name: "orders".into(), ..Default::default() });

let app = core
    .new_application()
    .on_server("api", move |shutdown| async move {
        let l = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
        axum::serve(l, build_api())
            .with_graceful_shutdown(shutdown.wait()) // drain on signal
            .await?;
        Ok(())
    })
    .on_server("admin", move |shutdown| async move {
        let l = tokio::net::TcpListener::bind("0.0.0.0:8081").await?;
        axum::serve(l, build_admin())
            .with_graceful_shutdown(shutdown.wait())
            .await?;
        Ok(())
    });

app.run().await?; // blocks until ctrl-c / SIGTERM, then drains
# Ok(())
# }
# fn build_api() -> axum::Router { axum::Router::new() }
# fn build_admin() -> axum::Router { axum::Router::new() }
```

Multiple servers are allowed (each gets its own task and drain signal), and
`on_start` / `on_stop` hooks let you run setup and teardown. A
`ShutdownHandle` (`app.shutdown_handle()`) triggers shutdown programmatically, so
integration tests never need to send a real signal.

## The management surface on a separate port

Always mount `core.actuator_router(..)` on a **different listener** from your
public API so `/actuator/*` (health, info, metrics, env, threaddump, …) is
reachable by your orchestrator and ops tooling but never exposed to the public
network. The Quickstart and the chapters above use `:8080` for the API and
`:8081` for the actuator — keep that split in production and firewall the admin
port.

Health endpoints feed your orchestrator's probes:

| Probe                | Endpoint                                     |
|----------------------|----------------------------------------------|
| liveness             | `/actuator/health/liveness`                  |
| readiness            | `/actuator/health/readiness`                 |
| overall              | `/actuator/health`                           |

## Production hardening middleware

The pyfly-parity middleware is OFF by default; turn on what you need via
`CoreConfig` and it weaves into `apply_middleware` at the correct filter order:

```rust,no_run
use firefly_starter_core::{Core, CoreConfig};
use firefly_web::{CorsConfig, SecurityHeadersConfig};

let core = Core::new(CoreConfig {
    app_name: "orders".into(),
    cors: Some(CorsConfig::permit_defaults()),         // CORS at the edge
    security_headers: Some(SecurityHeadersConfig::default()), // OWASP headers
    request_log: Some(Default::default()),             // one access-log event/request
    request_metrics: Some(Default::default()),         // http_server_requests_* metrics
    ..CoreConfig::default()
});
```

| Knob               | Adds                                                  |
|--------------------|-------------------------------------------------------|
| `cors`             | `CorsLayer` (preflight + simple-request decoration)   |
| `security_headers` | OWASP response headers (`nosniff`, `DENY`, HSTS, …)   |
| `csrf`             | double-submit-cookie CSRF                             |
| `request_log`      | one structured access-log event per request           |
| `request_metrics`  | `http_server_requests_seconds` + `_max` (actuator)    |
| `http_exchanges`   | recent-exchange recorder + `/actuator/httpexchanges`  |
| `loggers`          | `/actuator/loggers` runtime log-level control          |
| `redaction`        | PII scrubbing on the log writer                       |

The effective chain (outermost → innermost) is CORS → Problem →
SecurityHeaders → Correlation → Metrics → HttpExchanges → RequestLog → CSRF →
Idempotency → your router. Idempotency always stays innermost so a replayed
request still passes through every outer concern.

## TLS

`firefly-web`'s `Server` terminates TLS via `axum-server` + rustls. Bind under
`ServerProperties` with a `TlsConfig`:

```rust,ignore
use firefly_web::{Server, ServerProperties, TlsConfig};

let props = ServerProperties {
    host: "0.0.0.0".into(),
    port: 8443,
    tls: Some(TlsConfig { cert_file: "tls/cert.pem".into(), key_file: "tls/key.pem".into() }),
    ..Default::default()
};
// Server::serve(router, props, shutdown) honours the lifecycle drain.
```

Most deployments terminate TLS at an ingress/load balancer and run the service
plain-HTTP behind it; the built-in TLS is there for when you need end-to-end
encryption to the process.

## Configuration in production

Bind configuration from layered sources with environment overrides on top, so a
container reads its settings from the environment (see
[Configuration](./03-configuration.md)):

```bash
FIREFLY_PROFILE=prod \
FIREFLY_WEB_PORT=8080 \
DATABASE_URL=postgres://... \
  ./orders
```

`FIREFLY_*` variables beat the YAML files, secrets are masked in
`/actuator/env`, and `${...}` placeholders resolve env-then-config-then-default.
For dynamic reconfiguration, wire `ReloadableConfig` to `POST /actuator/refresh`.

## Container packaging

`firefly new` generates a `Dockerfile`. A typical multi-stage build:

```dockerfile
FROM rust:1.85 AS build
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=build /app/target/release/orders /usr/local/bin/orders
EXPOSE 8080 8081
ENTRYPOINT ["/usr/local/bin/orders"]
```

Because the framework traps SIGTERM and drains, the container stops cleanly when
the orchestrator sends a termination signal — no `--init` shim or
signal-forwarding wrapper is required.

## Selecting production adapters

Swap the in-process defaults for real infrastructure at the composition root —
nothing else changes:

```rust,ignore
use std::sync::Arc;
use firefly_starter_core::{Core, CoreConfig};

let core = Core::new(CoreConfig {
    app_name: "orders".into(),
    cache:  Some(Arc::new(redis_adapter)),  // firefly-cache-redis
    broker: Some(Arc::new(kafka_broker)),    // firefly-eda-kafka
    ..CoreConfig::default()
});
```

## A deployment checklist

- [ ] Actuator on a **separate, firewalled** port from the public API.
- [ ] Liveness/readiness probes pointed at `/actuator/health/{liveness,readiness}`.
- [ ] `security_headers`, `cors`, and (for browser flows) `csrf` enabled.
- [ ] `request_log` + `request_metrics` on, logs shipped as JSON, metrics scraped.
- [ ] Correlation propagation verified end-to-end across services.
- [ ] Idempotency store moved off in-memory to Redis/Postgres for multi-replica.
- [ ] TLS terminated (at ingress or in-process), secrets injected via environment.
- [ ] Graceful shutdown drain budget tuned for your slowest in-flight request.
- [ ] Real cache/broker/IDP adapters wired; the `make ci` gate green.

That completes the guided tour. The appendices follow: a
[Spring Boot migration map](./90-appendix-spring.md), the full
[Module Index](./91-appendix-modules.md), and the [Glossary](./92-glossary.md).
