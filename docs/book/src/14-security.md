# Security

`firefly-security` is the framework's **HTTP-layer authentication and
authorization tier**: a `Verifier` port for token validators, a `BearerLayer`
that authenticates requests, a path-prefix RBAC `FilterChain`, JWKS
verification, OAuth2 (client registrations, PKCE/OIDC login, an authorization
server), a role hierarchy, CSRF, and a bcrypt password encoder.

> **Spring parity** — This is Spring Security: a resource server (`BearerLayer` +
> JWKS), URL-based authorization (`FilterChain` ~ `HttpSecurity`), method
> guards, and an OAuth2 client + authorization server. Wire formats are RFC 7807
> problems byte-identical across the ports.

## The mental model

```text
                    incoming request
                            │
                            ▼
        ┌──────────────────────────────────────┐
        │           BearerLayer                 │
        │  • reads Authorization: Bearer <tok>  │
        │  • calls the Verifier (IDP adapter)   │
        │  • stores Authentication on request   │
        │  • 401 problem+json on failure        │
        └──────────────────────────────────────┘
                            │
                            ▼
        ┌──────────────────────────────────────┐
        │        FilterChain::layer()           │
        │  permit(prefix)             → public  │
        │  require(prefix, &[roles])  → RBAC    │
        │  401 / 403 problem+json on miss       │
        └──────────────────────────────────────┘
                            │
                            ▼
                       your handlers
             (read Extension<Authentication>)
```

## Authentication and the Verifier

A `Verifier` validates a bearer token and returns an `Authentication`
(principal + username + roles + claims). The `BearerLayer` extracts
`Authorization: Bearer <token>`, calls the verifier, and stores the result on
the request extensions. `VerifierFn` adapts a plain async closure:

```rust,no_run
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
    // axum runs the last-added layer first: bearer authenticates, then the
    // chain authorizes.
    .layer(chain.layer())
    .layer(BearerLayer::new(BearerConfig::new(verifier)));
```

Handlers read the authenticated principal with axum's `Extension`:

```rust,ignore
use axum::Extension;
use firefly_security::Authentication;

async fn admin_only(Extension(auth): Extension<Authentication>) -> String {
    if !auth.has_any_role(&["ADMIN", "OPERATOR"]) {
        return "forbidden".into();
    }
    format!("welcome {}", auth.username)
}
```

`Authentication::has_role`, `has_any_role`, and `has_authority` (which accepts a
role name or a fine-grained permission/scope) cover the common checks;
`Authentication::anonymous()` is the unauthenticated principal.

## Wiring an IDP adapter as the Verifier

An IDP adapter (`firefly_idp::Adapter`) exposes `validate(token) -> User`. Adapt
it to a `Verifier`:

```rust,ignore
use firefly_security::{Authentication, SecurityError, VerifierFn};

let verifier = VerifierFn(move |token: String| {
    let idp = idp_adapter.clone();
    async move {
        let u = idp.validate(&token).await
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

The IDP vendor adapters — Keycloak (OIDC + admin REST), Azure AD (Microsoft
Graph), AWS Cognito (JSON API + SigV4), and the self-hosted
`firefly-idp-internal-db` (bcrypt + HS256 JWT) — all satisfy the same `Adapter`
port, so this wiring is identical regardless of provider.

## URL authorization — the FilterChain

`FilterChain` matches request paths against rules. The Go-parity surface is
prefix-based (`permit` / `permit_method` / `require`); the pyfly-parity surface
adds an fnmatch-style glob DSL with **deny-by-default**:

```rust,ignore
use firefly_security::FilterChain;

let chain = FilterChain::new()
    .permit_pattern("/actuator/health")
    .permit_pattern("/public/**")
    .require_pattern("/api/admin/**", &["ADMIN"])
    .require_authority("/api/reports/**", "reports:read")
    .authenticated("/api/**")
    .any_request_authenticated(); // re-open the unmatched tail (or deny it)
```

> **Warning** — Once any rule is declared, the chain is **fail-closed**: a
> request matching no rule is rejected with 403 (Spring Security 6 semantics).
> Re-open the unmatched tail explicitly with `any_request_permit` /
> `any_request_authenticated` / `any_request_deny`. A chain with *no* rules at
> all is a no-op and passes everything through (never a blanket lockout).

## Role hierarchies and method guards

`RoleHierarchy` parses `"ADMIN > USER"` specs (transitive `expand`), consulted by
the `FilterChain` via `with_role_hierarchy` so granting `ADMIN` implies `USER`.

For method-level checks (the replacement for `@PreAuthorize` SpEL strings), the
`guards` module composes typed `AuthorizationGuard` predicates with `and` / `or`
/ `not`:

```rust,ignore
use firefly_security::guards::{has_authority, has_role};

let guard = has_role("ADMIN").or(has_authority("orders:write"));
// authorize is a method on the guard; pass the optional Authentication.
guard.authorize(Some(&auth))?; // 401 if no principal, 403 if predicate false
```

## JWKS verification

`JwksVerifier` is a JWKS resource-server `Verifier` (RS256, kid cache,
`iss`/`aud` validation, `exp` required). It maps claims to an `Authentication`:
`sub` → principal, `preferred_username`/`name` → username, a flat `roles` claim
or Keycloak `realm_access.roles` → roles, and `permissions` or a space-separated
`scope` → authorities. Drop it in wherever a `Verifier` is expected.

## OAuth2

The `oauth2` module covers both sides:

- **Client / login.** `ClientRegistration` with `google` / `github` /
  `keycloak` presets and an `InMemoryClientRegistrationRepository`.
  `OAuth2LoginHandler::router()` mounts the authorization-code flow — `state` +
  `nonce` + PKCE S256, OIDC id-token validation against the provider JWKS,
  userinfo fallback, and session-fixation-safe id rotation.
- **Authorization server.** `AuthorizationServer` issues HS256 JWTs for the
  `client_credentials` and `refresh_token` grants with refresh-token rotation
  and constant-time client authentication. The RFC-6749 error codes
  (`INVALID_CLIENT`, `UNAUTHORIZED_CLIENT`, `INVALID_GRANT`, …) match the other
  ports exactly.
- **Token storage.** A `TokenStore` port with `InMemoryTokenStore`,
  `RedisTokenStore` (`SET … EX`), and `PostgresTokenStore` implementations.

## CSRF

For cookie-session browser flows, `CsrfLayer` implements the double-submit
cookie pattern: an `XSRF-TOKEN` cookie compared against an `X-XSRF-TOKEN` header
on unsafe methods, safe-method pass-through with cookie refresh, a
`Authorization: Bearer` bypass (token clients do not need CSRF), and a 403
problem on mismatch. Mint and validate tokens directly with
`generate_csrf_token()` / `validate_csrf_token(cookie, header)`.

## Password hashing

`BcryptPasswordEncoder` is a standalone credential hash/verify primitive, usable
independently of any IDP (a worker, a custom user store, a rotation job):

```rust
use firefly_security::BcryptPasswordEncoder;

let enc = BcryptPasswordEncoder::new(); // work factor 12 (the default)
let hash = enc.hash("s3cret").unwrap();
assert!(enc.verify("s3cret", &hash).unwrap());
assert!(!enc.verify("wrong", &hash).unwrap());
```

The `$2b$` hashes interchange with the `firefly-idp-internal-db` adapter and the
Java/.NET/Go ports.

A secure service is also an observable one. The next chapter covers logging,
metrics, tracing, health, and the admin dashboard. Continue to
[Observability](./15-observability.md).
