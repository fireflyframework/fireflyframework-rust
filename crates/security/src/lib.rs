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

//! # firefly-security
//!
//! The framework's **HTTP-layer authentication and authorization tier**
//! — the Rust port of the Go `security` module (Java original: Spring
//! Security; .NET counterpart: `Microsoft.AspNetCore.Authentication.JwtBearer`).
//!
//! It provides four pieces:
//!
//! 1. [`Verifier`] — the authentication port for token validators (any
//!    IDP adapter satisfies it).
//! 2. [`BearerLayer`] — a tower layer that extracts
//!    `Authorization: Bearer <token>`, calls the [`Verifier`], and
//!    stores the resulting [`Authentication`] on the request.
//! 3. [`FilterChain`] — a path-prefix- and glob-pattern-keyed RBAC
//!    matcher composable with the bearer layer.
//! 4. [`Authentication`] — the principal + authorities tuple persisted
//!    on the request for downstream handlers and CQRS handlers alike.
//!
//! plus the **pyfly-parity layer** (Python original:
//! `pyfly.security`):
//!
//! 5. [`JwksVerifier`] — JWKS resource-server verifier (RS256, kid
//!    cache, iss/aud validation, Keycloak `realm_access.roles` and
//!    `scope` mapping).
//! 6. [`RoleHierarchy`] — `"ADMIN > USER"` implication graph consulted
//!    by the filter chain.
//! 7. CSRF double-submit helpers + [`CsrfLayer`].
//! 8. [`guards`] — typed authorization guards replacing pyfly's SpEL
//!    expressions.
//! 9. [`oauth2`] — client registrations (+ Google/GitHub/Keycloak
//!    presets), the browser login flow (auth-code + state/nonce +
//!    PKCE S256), and an authorization server (client_credentials +
//!    refresh_token) with pluggable token stores.
//! 10. [`PasswordEncoder`] + [`BcryptPasswordEncoder`] /
//!     [`Argon2PasswordEncoder`] — a standalone, reusable credential
//!     hash/verify primitive (pyfly's `pyfly.security.password` / Spring's
//!     `Argon2PasswordEncoder`), usable independently of any IdP.
//! 11. [`JwtService`] — a standalone symmetric (HMAC, HS256 default) JWT
//!     encode/decode/`to_authentication` primitive (pyfly's
//!     `pyfly.security.jwt.JWTService`), reusable for symmetric-token
//!     APIs, workers, and CLIs without any IdP; satisfies [`Verifier`].
//!
//! ## Mental model
//!
//! ```text
//!                     incoming request
//!                             │
//!                             ▼
//!         ┌──────────────────────────────────────┐
//!         │           BearerLayer                 │
//!         │  • reads Authorization: Bearer <tok>  │
//!         │  • calls Verifier (idp adapter)       │
//!         │  • stores Authentication on request   │
//!         │  • 401 application/problem+json on err│
//!         └──────────────────────────────────────┘
//!                             │
//!                             ▼
//!         ┌──────────────────────────────────────┐
//!         │        FilterChain::layer()           │
//!         │  permit(prefix)              → public │
//!         │  require(prefix, roles)      → RBAC   │
//!         │  401 / 403 problem+json on miss       │
//!         └──────────────────────────────────────┘
//!                             │
//!                             ▼
//!                        your handlers
//!              (read Extension<Authentication>)
//! ```
//!
//! ## Context propagation
//!
//! Where the Go port stores the [`Authentication`] on the request's
//! `context.Context`, the Rust port stores it in the request's
//! [`http::Extensions`] — axum handlers retrieve it with the
//! `Extension<Authentication>` extractor, or any middleware can use
//! [`authentication_from`] / [`must_auth_from`].
//!
//! ## Wire compatibility
//!
//! Rejections are emitted as RFC 7807 `application/problem+json`
//! envelopes with the canonical Firefly type URIs
//! (`https://fireflyframework.org/problems/unauthorized` and
//! `…/forbidden`) — byte-for-byte the same JSON the Java, .NET, Go,
//! and Python ports produce.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use axum::{routing::get, Extension, Router};
//! use firefly_security::{
//!     Authentication, BearerConfig, BearerLayer, FilterChain, SecurityError, VerifierFn,
//! };
//!
//! let verifier = VerifierFn(|token: String| async move {
//!     if token == "letmein" {
//!         Ok(Authentication {
//!             principal: "u1".into(),
//!             username: "alice".into(),
//!             roles: vec!["ADMIN".into()],
//!             ..Default::default()
//!         })
//!     } else {
//!         Err(SecurityError::verification("unknown token"))
//!     }
//! });
//!
//! let chain = FilterChain::new()
//!     .permit("/actuator/health")
//!     .permit("/actuator/info")
//!     .require("/admin/", &["ADMIN"])
//!     .require("/api/", &["USER", "ADMIN"]);
//!
//! let app: Router = Router::new()
//!     .route(
//!         "/admin/users",
//!         get(|Extension(auth): Extension<Authentication>| async move {
//!             format!("hello, {}", auth.username)
//!         }),
//!     )
//!     // Layers run outermost-last: bearer first, then the chain.
//!     .layer(chain.layer())
//!     .layer(BearerLayer::new(BearerConfig::new(verifier)));
//! ```

mod authentication;
mod bearer;
mod config;
mod context;
mod csrf;
mod filter_chain;
pub mod guards;
mod jwks;
mod jwt;
pub mod oauth2;
mod ott;
mod password;
mod problem;
mod role_hierarchy;
mod session_auth;

pub use authentication::{
    authentication_from, must_auth_from, with_authentication, Authentication, SecurityError,
    Verifier, VerifierFn, ANONYMOUS_ID, ROLE_PREFIX,
};
pub use bearer::{BearerConfig, BearerLayer, BearerService, UnauthorizedHandler};
pub use config::{
    bearer_layer_from_config, verifier_from_config, BearerProperties, JwtProperties,
    SecurityProperties,
};
pub use context::{
    check_access, current_authentication, with_authentication_scope,
    with_authentication_scope_sync, AccessRule,
};
pub use csrf::{
    generate_csrf_token, is_safe_method, validate_csrf_token, CookieSecure, CsrfLayer, CsrfService,
    CSRF_COOKIE_NAME, CSRF_HEADER_NAME, SAFE_METHODS,
};
pub use filter_chain::{FilterChain, FilterChainLayer, FilterChainService, Rule};
pub use guards::{require, AuthorizationGuard};
pub use jwks::{
    claims_to_authentication, Algorithm, JwksVerifier, DEFAULT_CLOCK_SKEW_SECONDS,
};
pub use jwt::{authentication_from_claims, JwtService, DEFAULT_EXPIRATION_SECONDS};
pub use ott::{
    ott_login_routes, InMemoryOneTimeTokenService, LoggingOttHandler, OneTimeToken,
    OneTimeTokenGenerationSuccessHandler, OneTimeTokenService, OttLoginState,
    DEFAULT_OTT_TTL_SECONDS,
};
pub use password::{Argon2PasswordEncoder, BcryptPasswordEncoder, PasswordEncoder, DEFAULT_ROUNDS};
pub use role_hierarchy::RoleHierarchy;
pub use session_auth::{
    SessionAuthenticationLayer, SessionAuthenticationService, SessionLoginSession,
    SessionLoginSessionStore,
};

/// Framework version stamp.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
