# `firefly-security`

> **Tier:** Platform · **Status:** Full · **Java original:** Spring Security · **Go module:** `security`

## Overview

`firefly-security` is the framework's **HTTP-layer authentication and
authorization tier**:

* `Verifier` — async port for token validators (any IDP adapter
  satisfies it); `VerifierFn` adapts plain async closures.
* `BearerLayer` — tower layer that extracts
  `Authorization: Bearer <token>`, calls the `Verifier`, and stores the
  resulting `Authentication` on the request extensions.
* `FilterChain` — path-prefix-keyed RBAC matcher composable with the
  bearer layer; `FilterChain::layer()` yields the tower layer.
* `Authentication` — principal + authorities tuple persisted on the
  request for downstream handlers and CQRS handlers alike.

## Mental model

```
                    incoming request
                            │
                            ▼
        ┌──────────────────────────────────────┐
        │           BearerLayer                 │
        │  • reads Authorization: Bearer <tok>  │
        │  • calls Verifier (idp adapter)       │
        │  • stores Authentication on request   │
        │  • 401 application/problem+json on err│
        └──────────────────────────────────────┘
                            │
                            ▼
        ┌──────────────────────────────────────┐
        │        FilterChain::layer()           │
        │  permit(prefix)              → public │
        │  permit_method(method, pfx)  → public │
        │  require(prefix, &[roles])   → RBAC   │
        │  401 / 403 problem+json on miss       │
        └──────────────────────────────────────┘
                            │
                            ▼
                       your handlers
             (read Extension<Authentication>)
```

## Design notes

* **Context propagation.** Where the Go port stores the
  `Authentication` on `context.Context`, the Rust port uses the
  request's `http::Extensions`. Handlers read it with axum's
  `Extension<Authentication>` extractor; middleware can use the
  Go-parity helpers `with_authentication` / `authentication_from` /
  `must_auth_from`.
* **Wire compatibility.** Rejections are RFC 7807
  `application/problem+json` envelopes with the canonical Firefly type
  URIs (`https://fireflyframework.org/problems/unauthorized`,
  `…/forbidden`) — the same bytes the Java, .NET, Go, and Python ports
  emit, including the Go sentinel `detail` strings
  (`firefly/security: unauthenticated`, `authentication required`,
  `required role missing`).
* **No nil panics.** Go's `BearerMiddleware` panics on a nil
  `Verifier`; `BearerConfig::new` takes the verifier up front so the
  invalid state is unrepresentable.
* **Errors.** `SecurityError` is a `thiserror` enum whose `Display`
  strings match the Go sentinel errors; verifier failures flow through
  `SecurityError::Verification` and surface verbatim as the problem
  `detail`.

## Public surface

```rust,ignore
pub struct Authentication {
    pub principal: String,            // unique stable id (sub claim)
    pub username: String,
    pub roles: Vec<String>,
    pub claims: HashMap<String, serde_json::Value>,
}
impl Authentication {
    pub fn has_role(&self, role: &str) -> bool;
    pub fn has_any_role(&self, roles: &[&str]) -> bool;
    pub fn anonymous() -> Self;
}

pub const ANONYMOUS_ID: &str = "anonymous";

#[async_trait]
pub trait Verifier: Send + Sync {
    async fn verify(&self, token: &str) -> Result<Authentication, SecurityError>;
}
pub struct VerifierFn<F>(pub F);          // adapts async closures

pub enum SecurityError { Unauthenticated, Forbidden, MalformedHeader, Verification(String) }

pub fn with_authentication<B>(req: Request<B>, auth: Authentication) -> Request<B>;
pub fn authentication_from<B>(req: &Request<B>) -> Option<&Authentication>;
pub fn must_auth_from<B>(req: &Request<B>) -> &Authentication;   // panics

pub struct BearerConfig { verifier, allow_anonymous, header_name, unauthorized }
pub struct BearerLayer;                   // BearerLayer::new(BearerConfig)

pub struct FilterChain;                   // FilterChain::new()
impl FilterChain {
    pub fn permit(self, prefix) -> Self;
    pub fn permit_method(self, method, prefix) -> Self;
    pub fn require(self, prefix, roles: &[&str]) -> Self;
    pub fn layer(self) -> FilterChainLayer;
}

// Standalone symmetric (HMAC) JWT primitive — pyfly JWTService.
pub struct JwtService;                    // JwtService::new(secret) → HS256
impl JwtService {
    pub fn algorithm(self, alg: Algorithm) -> Result<Self, SecurityError>; // HMAC only
    pub fn expiration_seconds(self, secs: u64) -> Self;
    pub fn encode(&self, payload: serde_json::Value) -> Result<String, SecurityError>;
    pub fn decode(&self, token: &str) -> Result<Map<String, Value>, SecurityError>; // exp required
    pub fn to_authentication(&self, token: &str) -> Result<Authentication, SecurityError>;
}
// + impl Verifier for JwtService

// Session-backed auth restore — pyfly OAuth2SessionSecurityFilter.
pub struct SessionAuthenticationLayer;    // ::new(); .anonymous_fallback(bool)
// Cookie-keyed firefly-session bridge for the OAuth2 login flow.
pub struct SessionLoginSessionStore;      // ::new(store) / ::from_config(cfg, store)
```

## Wiring with the IDP crates

`firefly_idp::Adapter` exposes `validate(token) -> Result<User, _>`.
Adapt it to a `Verifier`:

```rust,ignore
let verifier = VerifierFn(move |token: String| {
    let idp = idp_adapter.clone();
    async move {
        let u = idp
            .validate(&token)
            .await
            .map_err(|e| SecurityError::verification(e.to_string()))?;
        Ok(Authentication {
            principal: u.id,
            username: u.username,
            roles: u.roles,
            ..Default::default()
        })
    }
});
```

## Quick start

```rust
use axum::{routing::get, Extension, Router};
use firefly_security::{
    Authentication, BearerConfig, BearerLayer, FilterChain, SecurityError, VerifierFn,
};

let verifier = VerifierFn(|token: String| async move {
    if token == "letmein" {
        Ok(Authentication {
            principal: "u1".into(),
            username: "alice".into(),
            roles: vec!["ADMIN".into()],
            ..Default::default()
        })
    } else {
        Err(SecurityError::verification("unknown token"))
    }
});

let chain = FilterChain::new()
    .permit("/actuator/health")
    .permit("/actuator/info")
    .require("/admin/", &["ADMIN"])
    .require("/api/", &["USER", "ADMIN"]);

let app: Router = Router::new()
    .route(
        "/admin/users",
        get(|Extension(auth): Extension<Authentication>| async move {
            format!("hello, {}", auth.username)
        }),
    )
    // axum runs the last-added layer first: bearer, then the chain —
    // exactly Go's `bearer(chain.Middleware()(mux))`.
    .layer(chain.layer())
    .layer(BearerLayer::new(BearerConfig::new(verifier)));

// axum::serve(listener, app).await — as usual.
```

In handlers:

```rust,ignore
async fn admin_only(Extension(auth): Extension<Authentication>) -> Response {
    if !auth.has_any_role(&["ADMIN", "OPERATOR"]) {
        return forbidden_problem("must be admin/operator");
    }
    // ...
}
```

## Testing

```bash
cargo test -p firefly-security
```

Covers happy-path token verification, malformed-header 401, the
anonymous fallthrough mode, the filter-chain `permit / require /
forbidden` matrix, byte-exact problem+json wire shapes, custom header
and rejection-handler configuration, and `Send + Sync` bounds — all
in-process via `tower::ServiceExt::oneshot`.

## pyfly parity

On top of the Go-parity surface above, `firefly-security` ports the
full `pyfly.security` layer (Java: Spring Security OAuth2 resource
server + authorization server). Behaviour and wire formats match
pyfly; Python idioms are adapted to Rust (decorators → builders,
DI → explicit construction, `contextvars` → request extensions, SpEL
strings → typed predicates).

* **`JwksVerifier`** — JWKS resource-server `Verifier` (RS256, kid
  cache, `iss`/`aud` validation, `exp` required). Maps claims to
  `Authentication`: `sub` → principal, `preferred_username | name`
  → username, flat `roles` **or** Keycloak `realm_access.roles`
  → roles, `permissions` **or** space-separated `scope` → authorities.
  `claims_to_authentication` is reused for OIDC id-tokens.
* **`JwtService`** (pyfly `pyfly.security.jwt.JWTService`) — a
  standalone **symmetric** (HMAC) JWT primitive, the reusable
  counterpart to the RS256, verify-only `JwksVerifier`: HS256 default
  (HS384/HS512 configurable), `expiration_seconds` injecting an `exp`
  claim on `encode` when one is absent, `exp` **required** on `decode`
  (a never-expiring token is rejected), and `to_authentication`
  (pyfly's `to_security_context`: `sub` → principal,
  `roles`/`permissions` → roles/authorities). It satisfies the
  `Verifier` port, so it drops straight into `BearerLayer` for
  symmetric-token APIs, workers, CLIs, and inter-service tokens
  without any IdP. Errors carry pyfly's `Invalid token: <detail>`
  message shape; an asymmetric algorithm is rejected at construction.
* **`SessionAuthenticationLayer`** (pyfly
  `OAuth2SessionSecurityFilter`) — a tower layer that restores the
  `Authentication` stored on login from the request's
  `firefly_session::Session` (the `SECURITY_CONTEXT` attribute) into
  the request extensions, so `BearerLayer` / `FilterChain` / `guards`
  see the session-established principal. Inserts an anonymous context
  when no authenticated context is stored (toggle with
  `anonymous_fallback(false)`), closing the browser-login →
  authenticated-request loop. Mount it after `firefly_session::SessionLayer`.
* **`SessionLoginSessionStore`** — the production, cookie-keyed
  `LoginSessionStore` that resolves a per-browser
  `firefly_session::Session` from the session cookie and backs the
  OAuth2 login flow onto a real `firefly_session::SessionStore`
  (memory/Redis/Postgres). The multi-user replacement for the
  in-memory `FixedLoginSessionStore`; OAuth2 login and the subsequent
  authenticated requests share one distributed session.
* **`Authentication::authorities`** — fine-grained permissions/scopes,
  distinct from `roles`; `has_authority` accepts a role name or a
  permission (pyfly semantics).
* **`RoleHierarchy`** — parses `"ADMIN > USER"` specs (newline- or
  `;`-separated, transitive `expand`), consulted by `FilterChain` role
  and authority checks via `with_role_hierarchy`.
* **`FilterChain` URL DSL** (pyfly `HttpSecurity`) — `permit_pattern`,
  `require_pattern`, `require_authority`, `authenticated`, and `deny`
  use fnmatch-style globs (`/api/admin/**`); first matching rule wins.
  **Deny-by-default (fail-closed):** once any rule is declared, a
  request matching no rule is rejected with 403 — pyfly's
  deny-by-default (Spring Security 6) semantics. Re-open the unmatched
  tail with the catch-all `any_request_permit` /
  `any_request_authenticated` / `any_request_deny` (pyfly
  `any_request().permit_all()` etc.). A chain with no rules at all is a
  no-op and passes every request through (never a blanket lockout).
* **`guards`** — typed `AuthorizationGuard` predicates
  (`has_role` / `has_any_role` / `has_authority` / `has_any_authority`
  / `authenticated` / `permit_all` / `deny_all` plus `require(|auth|
  …)`) composed with `and` / `or` / `not`. The Rust replacement for
  pyfly's `@pre_authorize("hasRole('ADMIN')")` SpEL strings; `authorize`
  splits 401 (no/anonymous principal) from 403 (predicate false).
* **CSRF** — `generate_csrf_token` (43-char URL-safe), constant-time
  `validate_csrf_token`, and `CsrfLayer` (double-submit cookie:
  safe-method cookie issuance, `X-XSRF-TOKEN`/`XSRF-TOKEN` comparison
  on unsafe methods, `Authorization: Bearer` bypass, token rotation).
* **`oauth2`** module:
  * `ClientRegistration` + `google` / `github` / `keycloak` presets and
    the `ClientRegistrationRepository` port
    (`InMemoryClientRegistrationRepository`).
  * `OAuth2LoginHandler::router()` — axum routes for the
    authorization-code flow: `state` + `nonce` + PKCE S256, OIDC
    id-token validation against the provider JWKS, userinfo fallback,
    session-fixation-safe id rotation. Session state plugs in through
    the local `LoginSession` / `LoginSessionStore` traits so
    `firefly-session` (or any cookie store) can back it
    (`SessionLoginSessionStore` is the production bridge). With
    `OAuth2LoginHandler::with_concurrency(...)` it enforces a
    per-principal session cap (`maximumSessions`) at the login binding
    point — after the anti-fixation rotation it calls
    `SessionConcurrencyController::on_login`, returning `401 max_sessions`
    when the reject-new cap is reached and deregistering on
    `POST /logout` — matching pyfly's `OAuth2LoginAutoConfiguration`.
  * `AuthorizationServer` — `client_credentials` + `refresh_token`
    grants issuing HS256 JWTs with refresh-token rotation and
    constant-time client authentication. RFC-6749 error codes match
    pyfly exactly (`INVALID_CLIENT`, `UNAUTHORIZED_CLIENT`,
    `INVALID_REQUEST`, `INVALID_GRANT`, `UNSUPPORTED_GRANT_TYPE`).
  * `TokenStore` port + `InMemoryTokenStore`, `RedisTokenStore`
    (`SET … EX` / `GET` / `DEL`, configurable key prefix + TTL), and
    `PostgresTokenStore` (lazy table creation, SQL-identifier-validated
    table name).
* **`PasswordEncoder` + `BcryptPasswordEncoder`** (pyfly
  `pyfly.security.password`) — a standalone, reusable credential
  hash/verify primitive, usable independently of any IdP (a worker, a
  custom user store, a rotation job). `BcryptPasswordEncoder::new()` uses
  the pyfly default work factor (`DEFAULT_ROUNDS = 12`);
  `with_rounds(n)` mirrors pyfly's `rounds=`. `hash` / `verify` return a
  `Result<_, SecurityError>` (Rust surfaces a malformed stored hash as a
  value, not an exception); a correct-but-mismatching password is
  `Ok(false)`. Wire-compatible `$2b$` bcrypt hashes interchange with the
  `firefly-idp-internal-db` adapter and the Go/Java/.NET ports.

### pyfly-parity tests

* `jwks_test.rs` — `JwksVerifier` against an in-process axum JWKS
  server (real HTTP fetch path, kid cache, iss/aud, disallowed-alg
  rejection).
* `oauth2_test.rs` — `ClientRegistration` presets, repository, and the
  `AuthorizationServer` grant/error/rotation matrix.
* `oauth2_login_test.rs` — the full login flow against an in-process
  OAuth2 provider mock (token + userinfo + JWKS endpoints): PKCE, state
  mismatch, provider errors, verified-id-token vs userinfo paths.
* `session_auth_test.rs` — the `SessionAuthenticationLayer` restore loop
  end-to-end through `firefly_session::SessionLayer` (login stores a
  context, a later cookie-carrying request sees the restored principal;
  anonymous fallback on/off), the cookie-keyed `SessionLoginSessionStore`
  bridge resolving the same session by cookie across store instances, and
  the OAuth2 login → `SessionConcurrencyController` enforcement
  (reject-new `401 max_sessions`, admit-under-cap, logout deregistration).
* `persistent_token_store_test.rs` — `RedisTokenStore` round-trip with
  TTL against an in-process fake RESP server; `PostgresTokenStore`
  store / find / revoke round-trip env-gated on `FIREFLY_TEST_POSTGRES_URL`
  (fallback `DATABASE_URL` / `POSTGRES_URL`), skipping when unset and
  using a per-test table that is dropped afterwards when set.
* `pyfly_parity_test.rs` — `CsrfLayer` (pyfly `TestCsrfFilter`) and the
  `FilterChain` glob / `deny` / `authenticated` / `require_authority`
  / role-hierarchy behaviours through the real tower stack.
