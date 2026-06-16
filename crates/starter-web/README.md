# `firefly-starter-web`

> **Tier:** Starter · **Status:** Stable

## Overview

`firefly-starter-web` is the **web-tier starter**: it composes
[`firefly-starter-core`](../starter-core/) with the HTTP middleware bundle
switched **on** and an optional [`firefly-security`](../security/) filter
chain. It is the dedicated `WEB` starter — one of the five
(`core` / `web` / `application` / `data` / `domain`).

Where `Core` leaves the optional CORS / security-headers / access-log / CSRF
batteries **off** (so a non-HTTP worker / scheduler / CLI can opt out of the
web stack entirely — the reason the web tier is split out of `core`;
request metrics are on by default in both `Core` and `WebStack`),
`WebStack::new` additionally turns on CORS, security headers, and the
access-log by default:

| Battery | Wired by `WebStack::new` |
|---------|--------------------------|
| `CorsLayer` | permit-default CORS edge (`CorsConfig::permit_defaults`) |
| `SecurityHeadersLayer` | OWASP response headers (`X-Frame-Options`, `X-Content-Type-Options`, …) |
| `CorrelationLayer` | `X-Correlation-Id` (inherited, always on) |
| request metrics | `http_server_requests_seconds` timer bridged into the actuator `MetricRegistry` |
| request access-log | one structured `tracing` event per request |
| `IdempotencyLayer` | replay on `Idempotency-Key` (inherited, always on) |

So a consumer gets a **batteries-included HTTP service from a single
dependency**: `WebStack::apply_middleware` for the public router and
`WebStack::actuator_router` for the management surface (both inherited
from `Core` via `Deref`). The defaults are assembled from the same
`CoreConfig` knobs, so any of them can be overridden (or turned back off)
by passing an explicit value to `WebStack::new`.

`WebStack` dereferences to `Core`, so every core field and convenience
method — `actuator_router`, `new_application`, `print_banner`, the
admin-dashboard accessors, … — is available directly on the web value.
`starter_name` defaults to `"starter-web"`.

## Public surface

```rust,ignore
pub struct WebStack {
    pub core: Core,                  // Deref/DerefMut target
    pub security: Option<FilterChain>, // optional filter chain
    pub exception_advice: Option<firefly_web::ExceptionHandlerRegistry>, // optional global exception handlers
}

impl WebStack {
    pub fn new(cfg: CoreConfig) -> Self;              // batteries on
    pub fn with_security(self, chain: FilterChain) -> Self;
    pub fn set_security(&mut self, chain: FilterChain);   // in-place; for a FilterChain discovered as a DI bean
    pub fn set_exception_advice(&mut self, registry: firefly_web::ExceptionHandlerRegistry); // @ControllerAdvice handlers from DI
    pub fn apply_middleware(&self, router: Router) -> Router;
    // ... plus everything on Core via Deref
}
```

`Core`, `CoreConfig`, the web config types (`CorsConfig`,
`SecurityHeadersConfig`, `RequestMetricsConfig`) and the security types
(`FilterChain`, `FilterChainLayer`, `Rule`, `Authentication`,
`BearerConfig`, `BearerLayer`, `RoleHierarchy`, `Verifier`, `VerifierFn`,
`SecurityError`) are re-exported flat from this crate, so a web-tier
service can depend on `firefly-starter-web` alone.

## Quick start

```rust,ignore
use axum::{routing::get, Router};
use firefly_starter_web::{CoreConfig, FilterChain, WebStack};

#[tokio::main]
async fn main() {
    // Batteries-included web service from one dependency.
    let web = WebStack::new(CoreConfig {
        app_name: "orders-api".into(),
        app_version: "1.0.0".into(),
        ..CoreConfig::default()
    })
    .with_security(
        FilterChain::new()
            .permit("/actuator/")
            .any_request_permit(),
    );
    web.init_logging().unwrap();
    web.print_banner();

    let api = web.apply_middleware(
        Router::new().route("/orders", get(|| async { "[]" })),
    );
    let admin = web.actuator_router(Vec::new());
    // ... serve `api` and `admin` from a lifecycle Application ...
}
```

## Optional security filter chain

A `FilterChain` declares path-based access rules. Install one with
`with_security`; `apply_middleware` then layers it **inside** the
inherited core chain (just above your routes, after
correlation/CORS/security-headers have run) so a denied request is still
decorated with the security headers and a correlation id. Non-`allow`
rules expect an `Authentication` to have been populated upstream by a
`BearerLayer`; a chain of pure `permit`/`deny` rules needs no auth wiring.

## Testing

```bash
cargo test -p firefly-starter-web
```

Covers the wiring (batteries on, `"starter-web"` name, starter-name rules,
explicit CORS override winning over the permit-default), the headline **boot test**
(build the stack, mount a router through its middleware, oneshot a request
asserting OWASP security headers + CORS decoration + correlation id, plus
a CORS preflight short-circuit and an `/actuator/health` probe), the
optional security `FilterChain` (permit/deny gating with headers preserved
on a 403, a `require`-role route returning 401 unauthenticated, and
`with_security` last-wins replacement), `Deref`/`DerefMut` promotion of
the core surface, the inherited idempotency replay, and `Send + Sync`
bounds.
