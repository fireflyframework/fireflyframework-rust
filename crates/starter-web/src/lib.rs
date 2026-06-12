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

//! # firefly-starter-web
//!
//! The **web-tier starter**: [`firefly_starter_core`] with the web
//! middleware bundle switched **on** and an optional [`firefly_security`]
//! filter chain — the Rust spelling of pyfly's dedicated `WEB` starter
//! (one of the five: `core` / `web` / `application` / `data` / `domain`;
//! Java original: `firefly-starter-web`, .NET:
//! `FireflyFramework.Starter.Web`, `services.AddFireflyWeb`).
//!
//! Where [`Core`] leaves every pyfly-parity middleware knob **off** (so a
//! non-HTTP worker / scheduler / CLI can opt out of the web stack
//! entirely — exactly the rationale pyfly's `web.py` documents for
//! splitting the web tier out of `core`), [`WebStack::new`] flips the
//! HTTP batteries on by default:
//!
//! | Battery | Wired by [`WebStack::new`] |
//! |---------|----------------------------|
//! | [`CorsLayer`](firefly_web::CorsLayer) | Spring permit-default CORS edge ([`CorsConfig::permit_defaults`]) |
//! | [`SecurityHeadersLayer`](firefly_web::SecurityHeadersLayer) | OWASP response headers (`X-Frame-Options`, `X-Content-Type-Options`, …) |
//! | [`CorrelationLayer`](firefly_web::CorrelationLayer) | `X-Correlation-Id` (inherited, always on) |
//! | request metrics | `http_server_requests_seconds` timer bridged into the actuator [`MetricRegistry`] |
//! | request access-log | one structured `tracing` event per request |
//! | [`IdempotencyLayer`](firefly_web::IdempotencyLayer) | replay on `Idempotency-Key` (inherited, always on) |
//!
//! So a consumer gets a batteries-included HTTP service from a **single**
//! dependency: [`WebStack::apply_middleware`] for the public router and
//! [`WebStack::actuator_router`] for the management surface (both
//! inherited from [`Core`] via [`Deref`]). The defaults are assembled
//! from the same [`CoreConfig`] knobs, so any of them can be overridden
//! (or turned back off) by passing an explicit value to
//! [`WebStack::new`].
//!
//! ## Optional security filter chain
//!
//! A [`firefly_security::FilterChain`] declares path-based access rules
//! (pyfly's `HttpSecurity` / Spring Security 6 `SecurityFilterChain`).
//! Install one with [`WebStack::with_security`]; [`WebStack::apply_middleware`]
//! then layers it **inside** the inherited core chain (just above your
//! routes, after correlation/CORS/security-headers have run) so a denied
//! request is still decorated with the security headers and a correlation
//! id. Non-`allow` rules expect an [`Authentication`](firefly_security::Authentication)
//! to have been populated upstream by a
//! [`BearerLayer`](firefly_security::BearerLayer); a chain of pure
//! `permit`/`deny` rules needs no auth wiring.
//!
//! [`WebStack`] dereferences to [`Core`] (the Rust analog of Go's struct
//! embedding), so every core field (`bus`, `cache`, `broker`, `health`,
//! `metrics`, `scheduler`, …) and convenience method
//! ([`Core::new_application`], [`Core::print_banner`],
//! [`Core::actuator_router`], the admin-dashboard accessors, …) is
//! reachable directly on the web value. `starter_name` defaults to
//! `"starter-web"`.
//!
//! ## Quick start
//!
//! ```no_run
//! use axum::{routing::get, Router};
//! use firefly_starter_web::{CoreConfig, FilterChain, WebStack};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Batteries-included web service from one dependency.
//!     let web = WebStack::new(CoreConfig {
//!         app_name: "orders-api".into(),
//!         app_version: "1.0.0".into(),
//!         ..CoreConfig::default()
//!     })
//!     .with_security(
//!         FilterChain::new()
//!             .permit("/actuator/")
//!             .any_request_permit(),
//!     );
//!     web.init_logging()?;
//!     web.print_banner();
//!
//!     let api = web.apply_middleware(
//!         Router::new().route("/orders", get(|| async { "[]" })),
//!     );
//!     let admin = web.actuator_router(Vec::new());
//!
//!     let app = web
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

use std::ops::{Deref, DerefMut};

use axum::Router;

pub use firefly_security::{
    Authentication, BearerConfig, BearerLayer, FilterChain, FilterChainLayer, RoleHierarchy, Rule,
    SecurityError, Verifier, VerifierFn,
};
pub use firefly_starter_core::{
    Core, CoreConfig, CorsConfig, RequestMetricsConfig, SecurityHeadersConfig,
};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = firefly_starter_core::VERSION;

/// [`Core`] with the web middleware bundle on + an optional
/// [`FilterChain`] — the Rust spelling of pyfly's `WEB` starter (which
/// activates `pyfly.web.enabled` / `pyfly.server.enabled` /
/// `pyfly.observability.enabled` / `pyfly.web.actuator.enabled` and the
/// security-headers / CORS / OAuth2 filter chain).
///
/// Build it with [`WebStack::new`] (batteries on) and optionally attach a
/// security [`FilterChain`] with [`WebStack::with_security`]. The struct
/// [`Deref`]s to its embedded [`Core`], mirroring Go's
/// `*startercore.Core` embedding, so the whole core surface is reachable
/// directly.
pub struct WebStack {
    /// The wired infrastructure core, with the web middleware bundle
    /// (CORS / security headers / request metrics / access-log) enabled.
    /// [`WebStack`] also [`Deref`]s to this field.
    pub core: Core,
    /// The optional security filter chain layered around the public
    /// router by [`WebStack::apply_middleware`]; `None` until
    /// [`WebStack::with_security`] is called.
    pub security: Option<FilterChain>,
}

impl WebStack {
    /// Wires the web starter with the HTTP middleware bundle switched on —
    /// the Rust spelling of `services.AddFireflyWeb` / pyfly's
    /// `@enable_web_stack`.
    ///
    /// Delegates to [`Core::new`] for the infrastructure tier, but first
    /// fills in the web-tier defaults for any pyfly-parity knob the caller
    /// left unset:
    ///
    /// * `cors` → [`CorsConfig::permit_defaults`] (Spring `GET`/`HEAD`/`POST` set),
    /// * `security_headers` → [`SecurityHeadersConfig::default`] (OWASP headers),
    /// * `request_metrics` → [`RequestMetricsConfig::default`]
    ///   (`http_server_requests_seconds` bridged into the actuator registry),
    /// * `request_log` → [`RequestLogLayer`](firefly_web::RequestLogLayer)
    ///   (one structured access-log event per request).
    ///
    /// Each is applied with [`Option::get_or_insert_with`], so an explicit
    /// value passed in `cfg` always wins over the web-tier default — pass
    /// a customized [`CorsConfig`] / [`SecurityHeadersConfig`] to tune a
    /// battery (a battery cannot be turned fully *off* through this
    /// constructor; drop down to [`Core::new`] for an opt-out web tier).
    /// The `starter_name` resolves to `"starter-web"` unless the caller
    /// set a custom one (an explicit `"starter-core"`, being
    /// indistinguishable from the default after [`Core::new`], is renamed
    /// too — exactly like the sibling starters).
    pub fn new(mut cfg: CoreConfig) -> Self {
        // Web-tier batteries: on by default, but only fill in the gaps so
        // an explicit override in `cfg` is preserved.
        cfg.cors.get_or_insert_with(CorsConfig::permit_defaults);
        cfg.security_headers
            .get_or_insert_with(SecurityHeadersConfig::default);
        cfg.request_metrics
            .get_or_insert_with(RequestMetricsConfig::default);
        cfg.request_log
            .get_or_insert_with(firefly_starter_core::RequestLogLayer::new);

        let mut core = Core::new(cfg);
        if core.starter_name == "starter-core" {
            core.starter_name = "starter-web".to_string();
        }
        WebStack {
            core,
            security: None,
        }
    }

    /// Attaches a security [`FilterChain`] — pyfly's `HttpSecurity`
    /// `SecurityFilterChain`. [`WebStack::apply_middleware`] layers it
    /// just above your routes (inside the inherited correlation / CORS /
    /// security-headers edge), so a denied request still carries those
    /// response decorations. Replaces any previously attached chain.
    #[must_use]
    pub fn with_security(mut self, chain: FilterChain) -> Self {
        self.security = Some(chain);
        self
    }

    /// Wraps `router` in the full web middleware chain: the optional
    /// security [`FilterChain`] innermost (just above the routes), then
    /// the inherited [`Core::apply_middleware`] stack (idempotency →
    /// access-log → request-metrics → correlation → security headers →
    /// problem renderer → CORS edge).
    ///
    /// The security chain runs **inside** the core chain so that:
    ///
    /// * a request rejected by the chain (401/403) is still decorated with
    ///   the security headers and a correlation id, and
    /// * the access-log / metrics layers observe the rejected response.
    pub fn apply_middleware(&self, router: Router) -> Router {
        // Layer the security filter chain first so it sits closest to the
        // routes; `Core::apply_middleware` then wraps the whole thing in
        // the canonical correlation / headers / CORS edge.
        let router = match &self.security {
            Some(chain) => router.layer(chain.clone().layer()),
            None => router,
        };
        self.core.apply_middleware(router)
    }
}

impl Deref for WebStack {
    type Target = Core;

    fn deref(&self) -> &Core {
        &self.core
    }
}

impl DerefMut for WebStack {
    fn deref_mut(&mut self) -> &mut Core {
        &mut self.core
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use firefly_kernel::HEADER_CORRELATION_ID;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    fn web_for(app_name: &str) -> WebStack {
        WebStack::new(CoreConfig {
            app_name: app_name.into(),
            ..CoreConfig::default()
        })
    }

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    // ---- wiring (parity with pyfly test_web_decorator_injects_web_and_server) --

    /// The web starter flips the HTTP batteries on by default and names
    /// itself `"starter-web"`.
    #[test]
    fn web_stack_enables_batteries_by_default() {
        let w = web_for("orders");
        assert_eq!(w.app_name, "orders");
        assert_eq!(w.starter_name, "starter-web");
        // The web bundle is on (unlike a bare Core, where these are off).
        // Behavior is asserted in the boot test below; here we prove the
        // defaults flowed through Core::new with the cache wired live.
        assert_eq!(w.cache.name(), "memory");
        assert!(w.security.is_none());
    }

    /// Defaults fall back to the canonical names with the web tier's
    /// starter name (pyfly: the `WEB` bundle keys), like the siblings.
    #[test]
    fn defaults_fall_back_to_canonical_names() {
        let w = WebStack::new(CoreConfig::default());
        assert_eq!(w.app_name, "firefly-app");
        assert_eq!(w.starter_name, "starter-web");
        assert_eq!(w.log.service, "firefly-app");
    }

    /// A custom starter name passes through untouched; an explicit
    /// `"starter-core"` is renamed (it is indistinguishable from the
    /// default after `Core::new`) — exactly like the sibling starters.
    #[test]
    fn starter_name_rules_match_siblings() {
        let custom = WebStack::new(CoreConfig {
            starter_name: "starter-custom".into(),
            ..CoreConfig::default()
        });
        assert_eq!(custom.starter_name, "starter-custom");

        let core_named = WebStack::new(CoreConfig {
            starter_name: "starter-core".into(),
            ..CoreConfig::default()
        });
        assert_eq!(core_named.starter_name, "starter-web");
    }

    /// An explicit CORS / security-headers config passed in `cfg` wins
    /// over the web-tier defaults (`get_or_insert_with` only fills gaps).
    #[tokio::test]
    async fn explicit_cors_config_overrides_default() {
        let w = WebStack::new(CoreConfig {
            app_name: "orders".into(),
            cors: Some(CorsConfig {
                allowed_origins: vec!["https://app.example".into()],
                allowed_methods: vec!["GET".into()],
                allowed_headers: vec!["content-type".into()],
                ..CorsConfig::default()
            }),
            ..CoreConfig::default()
        });
        let app = w.apply_middleware(Router::new().route("/ping", get(|| async { "pong" })));
        // Disallowed origin is rejected at the edge — proving our explicit
        // origin list (not the permit-default `*`) is in force.
        let blocked = app
            .oneshot(
                Request::options("/ping")
                    .header("Origin", "https://evil.example")
                    .header("Access-Control-Request-Method", "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::BAD_REQUEST);
    }

    // ---- boot test: build the stack, mount a router, oneshot a request ----

    /// The headline boot test: build the web stack, mount a router through
    /// its middleware, and oneshot a request asserting security headers +
    /// CORS decoration + a correlation id; then probe an actuator route.
    #[tokio::test]
    async fn boot_security_headers_cors_and_actuator() {
        let web = WebStack::new(CoreConfig {
            app_name: "orders".into(),
            app_version: "1.0.0".into(),
            ..CoreConfig::default()
        });

        let api = web.apply_middleware(
            Router::new().route("/orders", get(|| async { (StatusCode::OK, "order") })),
        );
        let admin = web.actuator_router(Vec::new());

        // 1. A real GET carries the OWASP security headers, the CORS
        //    origin decoration (permit-default `*`), and a correlation id.
        let res = api
            .clone()
            .oneshot(
                Request::get("/orders")
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
            "*"
        );
        assert!(res.headers().contains_key(HEADER_CORRELATION_ID));

        // 2. CORS preflight is short-circuited at the edge with the
        //    permit-default allow-* set.
        let preflight = api
            .clone()
            .oneshot(
                Request::options("/orders")
                    .header("Origin", "https://app.example")
                    .header("Access-Control-Request-Method", "POST")
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
            "*"
        );

        // 3. The actuator surface answers /actuator/health UP with the
        //    default cache probe.
        let res = admin
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
    }

    // ---- security filter chain --------------------------------------------

    /// A pure permit/deny `FilterChain` (no auth wiring) gates routes:
    /// a denied path is 403 — still decorated with the security headers,
    /// proving the chain runs inside the core edge.
    #[tokio::test]
    async fn security_filter_chain_denies_and_keeps_headers() {
        let web = web_for("orders").with_security(
            FilterChain::new()
                .permit("/public/")
                .deny("/private/**")
                .any_request_permit(),
        );
        assert!(web.security.is_some());

        let app = web.apply_middleware(
            Router::new()
                .route("/public/ping", get(|| async { "pong" }))
                .route("/private/secret", get(|| async { "secret" })),
        );

        // Permitted path passes through.
        let ok = app
            .clone()
            .oneshot(Request::get("/public/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);

        // Denied path is rejected, but the security headers (applied by
        // the outer core chain) still decorate the 403.
        let denied = app
            .oneshot(Request::get("/private/secret").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(denied.headers().get("x-frame-options").unwrap(), "DENY");
    }

    /// A `require`-role chain without an upstream `BearerLayer` rejects
    /// the protected route as 401 (no `Authentication` populated), exactly
    /// like pyfly's `HttpSecurity` fail-closed default.
    #[tokio::test]
    async fn security_require_role_unauthenticated_is_401() {
        let web = web_for("orders").with_security(
            FilterChain::new()
                .permit("/login")
                .require("/admin/", &["ADMIN"]),
        );
        let app = web.apply_middleware(
            Router::new()
                .route("/login", get(|| async { "form" }))
                .route("/admin/users", get(|| async { "users" })),
        );

        let res = app
            .oneshot(Request::get("/admin/users").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    /// `with_security` replaces a previously attached chain (last wins).
    #[test]
    fn with_security_replaces_previous_chain() {
        let web = web_for("orders")
            .with_security(FilterChain::new().permit("/a"))
            .with_security(FilterChain::new().deny("/b").any_request_permit());
        let chain = web.security.as_ref().unwrap();
        // The second chain's first rule is the deny on `/b`.
        assert_eq!(chain.rules().len(), 2);
    }

    // ---- Deref promotion + invariants -------------------------------------

    /// Deref promotes the embedded core's surface, mirroring Go's field
    /// and method promotion through the embedded `*startercore.Core`.
    #[test]
    fn deref_promotes_core_surface() {
        let web = web_for("billing");
        let banner = web.banner();
        assert!(banner.contains("billing"));
        assert!(banner.contains("starter-web"));
        assert_eq!(web.new_application().name(), "billing");

        // DerefMut allows post-construction tweaks on core fields.
        let mut web = web;
        web.app_version = "2.0.0".into();
        assert_eq!(web.core.app_version, "2.0.0");
    }

    /// The idempotency battery (inherited, always on) replays a repeated
    /// `Idempotency-Key`, proving the inherited core chain runs under the
    /// web stack's `apply_middleware`.
    #[tokio::test]
    async fn inherited_idempotency_replays() {
        use axum::response::IntoResponse;
        use axum::routing::post;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let hits = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&hits);
        let web = web_for("orders");
        let app = web.apply_middleware(Router::new().route(
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
        // Drain the first body so the idempotency record is persisted
        // (the capture tee stores it once the last frame is polled).
        let first_body = first.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&first_body[..], b"order-1");

        let second = app.oneshot(request()).await.unwrap();
        assert_eq!(second.headers().get("Idempotent-Replay").unwrap(), "true");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "handler ran exactly once");
    }

    #[test]
    fn version_matches_workspace() {
        assert_eq!(VERSION, firefly_starter_core::VERSION);
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn web_stack_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WebStack>();
    }
}
