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
