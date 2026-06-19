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
mod authentication_manager;
mod bearer;
mod config;
mod context;
mod csrf;
mod exception;
mod filter_chain;
mod form_login;
pub mod guards;
mod http_basic;
mod jwks;
mod jwt;
#[cfg(feature = "ldap")]
mod ldap;
pub mod oauth2;
mod ott;
mod password;
mod permission;
mod problem;
mod remember_me;
mod request_cache;
mod role_hierarchy;
#[cfg(feature = "saml2")]
mod saml2;
mod security_context;
mod security_filter_chains;
mod session_auth;
mod session_policy;
mod userdetails;
#[cfg(feature = "webauthn")]
mod webauthn;

pub use authentication::{
    authentication_from, must_auth_from, with_authentication, Authentication, SecurityError,
    Verifier, VerifierFn, ANONYMOUS_ID, ROLE_PREFIX,
};
pub use authentication_manager::{
    AuthenticationEvent, AuthenticationEventPublisher, AuthenticationManager,
    AuthenticationProvider, AuthenticationRequest, BearerTokenAuthenticationProvider,
    LoggingAuthenticationEventPublisher, ProviderManager,
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
pub use exception::{
    AccessDeniedHandler, AuthenticationEntryPoint, BasicAuthenticationEntryPoint,
    ProblemAccessDeniedHandler, ProblemAuthenticationEntryPoint,
};
pub use filter_chain::{FilterChain, FilterChainLayer, FilterChainService, Rule};
pub use form_login::{
    form_login_routes, FormLoginFailureHandler, FormLoginState, FormLoginSuccessHandler,
};
pub use guards::{require, AuthorizationGuard};
pub use http_basic::{HttpBasicLayer, HttpBasicService};
pub use jwks::{claims_to_authentication, Algorithm, JwksVerifier, DEFAULT_CLOCK_SKEW_SECONDS};
pub use jwt::{authentication_from_claims, JwtService, DEFAULT_EXPIRATION_SECONDS};
#[cfg(feature = "ldap")]
pub use ldap::{
    cn_from_dn, escape_filter_value, ActiveDirectoryLdapAuthenticationProvider, Ldap3Operations,
    LdapAuthenticationProvider, LdapEntry, LdapOperations,
};
pub use ott::{
    ott_login_routes, InMemoryOneTimeTokenService, LoggingOttHandler, OneTimeToken,
    OneTimeTokenGenerationSuccessHandler, OneTimeTokenService, OttLoginState,
    DEFAULT_OTT_TTL_SECONDS,
};
pub use password::{
    Argon2PasswordEncoder, BcryptPasswordEncoder, DelegatingPasswordEncoder, NoOpPasswordEncoder,
    PasswordEncoder, DEFAULT_PASSWORD_ENCODER_ID, DEFAULT_ROUNDS,
};
pub use permission::{has_permission, set_permission_evaluator, PermissionEvaluator};
pub use remember_me::{
    RememberMeServices, TokenBasedRememberMeServices, DEFAULT_REMEMBER_ME_SECONDS,
};
pub use request_cache::{
    HttpSessionRequestCache, NullRequestCache, RequestCache, SavedRequest,
    SESSION_KEY_SAVED_REQUEST,
};
pub use role_hierarchy::RoleHierarchy;
#[cfg(feature = "saml2")]
pub use saml2::{
    AllowedSignatureAlgorithm, AssertionReplayCache, AuthnRedirect, InMemoryAssertionReplayCache,
    InMemoryRelyingPartyRegistrationRepository, InMemorySaml2AuthenticationRequestRepository,
    RelyingPartyRegistration, RelyingPartyRegistrationBuilder, RelyingPartyRegistrationRepository,
    Saml2AuthenticationRequestRepository,
};
pub use security_context::{
    HttpSessionSecurityContextRepository, NullSecurityContextRepository, SecurityContextRepository,
};
pub use security_filter_chains::{
    AnyRequestMatcher, PathRequestMatcher, RequestMatcher, SecurityFilterChains,
    SecurityFilterChainsLayer, SecurityFilterChainsService,
};
pub use session_auth::{
    SessionAuthenticationLayer, SessionAuthenticationService, SessionLoginSession,
    SessionLoginSessionStore,
};
pub use session_policy::SessionCreationPolicy;
pub use userdetails::{
    AccountStatusUserDetailsChecker, DaoAuthenticationProvider, InMemoryUserDetailsService,
    UserDetails, UserDetailsChecker, UserDetailsService,
};
#[cfg(feature = "webauthn")]
pub use webauthn::{
    webauthn_routes, CeremonyStateStore, InMemoryCeremonyStore, InMemoryPasskeyRepository,
    InMemoryUserEntityRepository, PasskeyCredentialRepository,
    PublicKeyCredentialUserEntityRepository, WebAuthnError, WebAuthnProperties,
    WebAuthnRelyingParty, WebAuthnState,
};

/// Framework version stamp.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Builds the `reqwest` client used for the security tier's outbound calls
/// (OAuth2 token / introspection / userinfo / JWKS endpoints) with sane
/// timeouts, so a slow, half-open, or hostile endpoint cannot hang the request
/// indefinitely — important because token introspection sits on the inbound
/// bearer-verification hot path. A timeout surfaces as a fail-closed error.
pub(crate) fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// The maximum response body (bytes) the security tier will buffer from an
/// OAuth2 endpoint before parsing — an RFC 7662 / RFC 6749 response is tiny, so
/// this caps a hostile endpoint's memory amplification while leaving ample room.
pub(crate) const MAX_OAUTH2_RESPONSE_BYTES: u64 = 1 << 20; // 1 MiB
