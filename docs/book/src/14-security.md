# Security

In Chapter 13 you saw how Lumen *would* call an external payments or FX
provider. Lumen itself, though, is still wide open: any caller can open a
wallet, deposit, withdraw, or move money between wallets. Before Part V can ship
Lumen to production, you have to close that door.

By the end of this chapter Lumen will **authenticate** every request with a
signed JWT, **authorize** the mutating routes with a path-based RBAC filter
chain, and leave the public reads and the management surface open. The whole
thing is built on `firefly-security`, reached through the one `firefly` facade —
no new dependency, no hand-rolled crypto, and (true to Lumen's promise) not even
a `thiserror` derive.

> **Spring parity.** `firefly-security` is Spring Security: a resource server
> (`BearerLayer` + a `Verifier`, the analog of a JWT decoder), URL-based
> authorization (`FilterChain` ~ `HttpSecurity.authorizeHttpRequests()`), method
> guards, JWKS validation, an OAuth2 client + authorization server, a role
> hierarchy, CSRF, and a bcrypt encoder. The wire formats — RFC 9457 problems on
> a 401/403 — are byte-identical across the Java, .NET, Go, and Python ports.

## The mental model

```text
                    incoming request
                            │
                            ▼
        ┌──────────────────────────────────────┐
        │            BearerLayer                │
        │  • reads Authorization: Bearer <tok>  │
        │  • calls the Verifier (JwtService)    │
        │  • stores Authentication on request   │
        │  • allow_anonymous → pass empty ctx   │
        └──────────────────────────────────────┘
                            │
                            ▼
        ┌──────────────────────────────────────┐
        │           FilterChain (RBAC)          │
        │  permit_method("GET", "/api/v1/...")  │
        │  permit("/actuator/")                 │
        │  require("/api/v1/wallets", CUSTOMER) │
        │  401 / 403 problem+json on a miss     │
        └──────────────────────────────────────┘
                            │
                            ▼
                  WalletApi handlers
```

`firefly-security` carries far more than Lumen uses — JWKS, OAuth2 (client +
authorization server), `RoleHierarchy`, method-level `guards`,
`CsrfLayer`, `BcryptPasswordEncoder`. We tour those at the end; first, the four
pieces Lumen actually wires.

## Minting and verifying tokens — JwtService

Lumen is a stateless API. Sessions would need sticky routing or a shared store
on every replica; a signed JWT lets each request carry its own credential, so
the service scales horizontally with no shared state. The framework's
`JwtService` both **mints** the demo tokens and **verifies** the incoming ones,
using a symmetric HS256 key — which is exactly what makes Lumen runnable and
testable with no external IdP.

Here is the whole of Lumen's token surface, from `src/security.rs`:

```rust,ignore
use firefly::security::{
    BearerConfig, BearerLayer, FilterChain, JwtService, SecurityError, Verifier, VerifierFn,
};
use serde_json::json;

/// The demo signing key. A real service reads this from configuration / a
/// secret store; it is inlined here so the sample is runnable as-is.
pub const DEMO_SIGNING_KEY: &[u8] = b"lumen-demo-signing-key-change-me";

/// The role every mutating wallet command requires.
pub const CUSTOMER_ROLE: &str = "CUSTOMER";

/// The shared HS256 service that both signs the demo tokens and verifies
/// incoming bearer tokens.
fn jwt_service() -> JwtService {
    JwtService::new(DEMO_SIGNING_KEY)
}

/// Mints a signed HS256 access token for `subject` with `roles`, valid for the
/// service's default lifetime (one hour).
pub fn mint_token(subject: &str, roles: &[&str]) -> String {
    jwt_service()
        .encode(json!({ "sub": subject, "roles": roles }))
        .expect("mint_token: HS256 encode")
}
```

`JwtService::new(secret)` builds an HS256 service. `encode` signs a JSON
payload and — this is the load-bearing detail — **injects an `exp` claim** when
the payload has none, defaulting to one hour out. Every token Lumen issues has a
bounded lifetime; a token with no `exp` is rejected at `decode` time. The
`mint_token` helper is what the HTTP tests call to obtain a credential the
verifier will accept.

> **Spring parity.** `JwtService.encode` / `decode` / `to_authentication` mirror
> pyfly's `JWTService.encode` / `decode` / `to_security_context`, which in turn
> wrap Spring's `JwtEncoder` / `JwtDecoder`. The mandatory `exp` claim matches
> pyfly's `options={"require": ["exp"]}` and Spring's default decoder validation.

## The Verifier and the Authentication

A `Verifier` is the resource-server port: validate a token, return an
`Authentication` (the principal, username, roles, and the raw claims).
`VerifierFn` adapts a plain async closure into one. Lumen's verifier delegates
straight to `JwtService::to_authentication`, which maps `sub` → principal and
`roles` → roles, then re-wraps any failure as a `SecurityError::Verification`:

```rust,ignore
/// Builds the resource-server Verifier: validates the token's HS256
/// signature + expiry, then maps `sub` → principal and `roles` → roles onto an
/// Authentication. A bad signature / expired token surfaces as a
/// SecurityError::Verification, which the BearerLayer renders as a
/// `401 application/problem+json`.
pub fn build_verifier() -> impl Verifier {
    VerifierFn(|token: String| async move {
        jwt_service()
            .to_authentication(&token)
            .map_err(|e: SecurityError| SecurityError::verification(format!("invalid token: {e}")))
    })
}
```

`Authentication` carries the fields a handler or filter inspects:

| Field       | Type                              | From the claim                |
|-------------|-----------------------------------|-------------------------------|
| `principal` | `String`                          | `sub`                         |
| `username`  | `String`                          | `sub` (or a friendlier claim) |
| `roles`     | `Vec<String>`                     | `roles`                       |
| `claims`    | `HashMap<String, serde_json::Value>` | every decoded claim        |

Its helpers — `has_role(r)`, `has_any_role(&[..])`, `has_authority(a)` (matches a
role *or* a fine-grained permission/scope), and `Authentication::anonymous()` —
cover the common checks. Lumen's unit test asserts the round-trip directly:

```rust,ignore
#[tokio::test]
async fn mint_then_verify_roundtrips_claims() {
    let token = mint_token("u-alice", &[CUSTOMER_ROLE]);
    let auth: Authentication = build_verifier().verify(&token).await.unwrap();
    assert_eq!(auth.principal, "u-alice");
    assert!(auth.has_role(CUSTOMER_ROLE));
}
```

A tampered token (`"not.a.jwt"`) or one signed with the wrong key is rejected
with `SecurityError::Verification` — the two negative tests in `security.rs`
prove it.

> **Production swap.** Move to a real identity provider by replacing
> `build_verifier` with `firefly::security::JwksVerifier`, pointed at your IdP's
> JWKS URI (RS256, `kid` cache, `iss`/`aud` validation, `exp` required). The
> `Verifier` port is identical, so `security_layers` — and every handler — is
> untouched. That is the "swap the adapter, keep the code" promise applied to
> identity.

## URL authorization — the FilterChain

`JwtService` answers *who is this caller?*; the `FilterChain` answers *may they
do this?* It matches request paths against rules in declaration order — first
match wins — and renders a 401 (no/invalid credential) or 403 (authenticated but
under-privileged) as an RFC 9457 problem. Lumen composes the bearer layer and
the chain in one place:

```rust,ignore
/// Builds the BearerLayer + FilterChain that protect the service.
///
/// | Route                                          | Rule                  |
/// |------------------------------------------------|-----------------------|
/// | `GET  /api/v1/wallets/:id`                      | permit (public read)  |
/// | `GET  /actuator/*`                              | permit (management)   |
/// | `POST /api/v1/wallets`                          | require `CUSTOMER`    |
/// | `POST /api/v1/wallets/:id/deposit` / `withdraw` | require `CUSTOMER`    |
/// | `POST /api/v1/transfers`                        | require `CUSTOMER`    |
pub fn security_layers() -> (BearerLayer, FilterChain) {
    // `allow_anonymous` lets an unauthenticated request reach the chain; the
    // chain (not the bearer layer) then decides — a 401 on a `require` route
    // without a valid token, a pass on a permitted route.
    let bearer = BearerLayer::new(BearerConfig::new(build_verifier()).allow_anonymous(true));
    let chain = FilterChain::new()
        .permit_method("GET", "/api/v1/wallets")
        .permit("/actuator/")
        .require("/api/v1/wallets", &[CUSTOMER_ROLE])
        .require("/api/v1/transfers", &[CUSTOMER_ROLE])
        .any_request_permit();
    (bearer, chain)
}
```

Two design choices are worth dwelling on:

- **`allow_anonymous(true)` on the bearer layer.** With it set, a request with
  no `Authorization` header is *not* rejected at the bearer layer — it reaches
  the chain carrying an anonymous `Authentication`. That keeps a single
  decision-maker: the `FilterChain` decides every route. A public `GET` passes;
  a `require` route with no valid token becomes a 401. Without
  `allow_anonymous`, the bearer layer would reject anonymous traffic before the
  chain could permit the public reads.
- **Order matters.** `permit_method("GET", "/api/v1/wallets")` and
  `permit("/actuator/")` come *first*, so the public reads and the management
  surface are decided before the broad `require("/api/v1/wallets", ...)` could
  catch them. `any_request_permit()` re-opens the unmatched tail.

> **Warning.** Once any rule is declared, a `FilterChain` is **fail-closed**: a
> request matching no rule is rejected with 403 (Spring Security 6 semantics).
> Re-open the unmatched tail explicitly with `any_request_permit()` /
> `any_request_authenticated()` / `any_request_deny()`. A chain with *no* rules is
> a no-op and passes everything (never a surprise blanket lockout).

## Layering it onto the router

The chain is attached to the `WebStack` at build time, and the bearer layer is
applied around the whole router so the chain sees a populated `Authentication`.
This is the relevant slice of `LumenApp::router` and `build_app` in `src/web.rs`:

```rust,ignore
// in build_app():
let (_bearer, chain) = crate::security::security_layers();
let web = WebStack::new(firefly::starter_web::CoreConfig {
    app_name: APP_NAME.into(),
    app_version: VERSION.into(),
    ..Default::default()
})
.with_security(chain);

// in LumenApp::router():
let (bearer, _chain) = crate::security::security_layers();
// The WebStack carries the FilterChain (set in build_app); layer the
// bearer auth on the outside so the chain sees a populated Authentication.
self.web.apply_middleware(routes).layer(bearer)
```

`WebStack::with_security(chain)` stores the chain; `WebStack::apply_middleware`
layers it *inside* the inherited correlation / security-headers / CORS edge, so
even a 401 response carries those headers and a correlation id. The bearer layer
goes on the outside (axum runs the last-added layer first): authenticate, then
authorize.

## Proving it end to end

The HTTP suite (`tests/http.rs`) drives the fully-wired router with
`tower::oneshot` and asserts the security behavior directly. A mutation with no
token is a 401 problem; a malformed body on an authenticated request is a 422
problem:

```rust,ignore
#[tokio::test]
async fn missing_token_is_401_problem_on_mutations() {
    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "mallory", "openingBalance": 10 }),
            false, // no Authorization header
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(content_type(&res).contains("application/problem+json"));
}
```

The test helper builds the `Authorization` header from `mint_token`, so an
authenticated request is just `post(path, body, true)`:

```rust,ignore
fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &[CUSTOMER_ROLE]))
}
```

Every other test in the suite — open, get, deposit/withdraw, transfer — runs as
the minted `CUSTOMER`, so authentication is exercised on the happy path too.

## The rest of firefly-security (Lumen's growth room)

Lumen uses the symmetric-key fast path. The same crate carries the production
surface you reach for as a real wallet service matures:

- **JWKS verification.** `JwksVerifier` is a drop-in `Verifier` for RS256 tokens
  from an external IdP (Keycloak, Auth0, Cognito): `kid` cache, `iss`/`aud`
  checks, `exp` required, and the same `sub`/`roles`/`permissions` claim mapping.
- **Method guards.** For per-handler checks (the analog of `@PreAuthorize`), the
  `guards` module composes typed predicates: `has_role("CUSTOMER")
  .or(has_authority("wallet:approve"))`, then `guard.authorize(Some(&auth))?` —
  401 with no principal, 403 if the predicate is false.
- **Role hierarchy.** `RoleHierarchy::new()` parses `"ADMIN > CUSTOMER"` specs;
  attach it with `chain.with_role_hierarchy(..)` so granting `ADMIN` implies
  `CUSTOMER`.
- **Pattern rules.** Alongside the prefix rules Lumen uses, the chain offers an
  fnmatch-style glob DSL — `permit_pattern("/public/**")`,
  `require_pattern("/api/admin/**", &["ADMIN"])`,
  `require_authority("/api/reports/**", &["reports:read"])`,
  `authenticated("/api/**")`.
- **Sessions.** For browser flows where logout must mean logout, the
  `firefly-session` crate adds a `SessionLayer` over a `SessionStore`
  (`MemorySessionStore` for dev, a Redis-backed store for scale). A handler pulls
  the request's `Session` with the `SessionExt` extractor and calls
  `session.rotate_id()` after login (session-fixation defense),
  `session.set_attribute("user_id", id)`, and `session.invalidate()` on logout.
- **OAuth2.** The `oauth2` module covers both sides: `ClientRegistration` +
  `OAuth2LoginHandler` for the authorization-code login flow (state + nonce +
  PKCE S256, OIDC id-token validation), and an `AuthorizationServer` that issues
  tokens for `client_credentials` / `refresh_token`.
- **CSRF & passwords.** `CsrfLayer` implements the double-submit-cookie pattern
  for cookie-session flows; `BcryptPasswordEncoder` (default work factor 12)
  hashes credentials. The `$2b$` hashes interchange with the
  `firefly-idp-internal-db` adapter and every other port.

```rust
use firefly_security::BcryptPasswordEncoder;

let enc = BcryptPasswordEncoder::new(); // work factor 12 (the default)
let hash = enc.hash("s3cret").unwrap();
assert!(enc.verify("s3cret", &hash).unwrap());
assert!(!enc.verify("wrong", &hash).unwrap());
```

## What changed in Lumen

This chapter closed Lumen's open front door without adding a dependency or a
line of business logic to the handlers:

- A single HS256 **`JwtService`** (`src/security.rs`) both mints the demo tokens
  and verifies incoming ones; `encode` auto-stamps a one-hour `exp`, and a token
  without `exp` is rejected at decode time.
- **`build_verifier`** turns the service into a `Verifier` via `VerifierFn`,
  mapping `sub` → principal and `roles` → roles onto an `Authentication`, with a
  bad token surfacing as `SecurityError::Verification` → a 401 problem.
- **`security_layers`** composes a `BearerLayer` (with `allow_anonymous(true)`)
  and a path-ordered RBAC `FilterChain`: public `GET /api/v1/wallets/:id`,
  public `/actuator/*`, and `CUSTOMER`-only `POST /api/v1/wallets`,
  `/deposit`, `/withdraw`, and `/transfers`.
- **`WebStack::with_security` + `apply_middleware`** attach the chain inside the
  correlation/headers edge; `LumenApp::router` layers the bearer auth on the
  outside.
- The HTTP suite proves the contract: an unauthenticated mutation is a 401
  problem, the happy paths run as a minted `CUSTOMER`, and the unit tests
  round-trip and reject tokens.

## Exercises

1. **Add an ADMIN-only route.** Give Lumen a hypothetical `GET
   /api/v1/wallets` collection list and protect it with `require_pattern` so
   only an `ADMIN` may list every wallet, while `CUSTOMER` keeps access to the
   single-wallet read. Mint an `ADMIN` token in a test and assert a `CUSTOMER`
   token gets a 403.
2. **Role hierarchy.** Introduce a `SUPER` role that implies `CUSTOMER`. Build a
   `RoleHierarchy` from `"SUPER > CUSTOMER"`, attach it with
   `chain.with_role_hierarchy(..)`, mint a `SUPER`-only token, and assert it
   passes the `require("/api/v1/wallets", &["CUSTOMER"])` rule.
3. **Swap in JWKS.** Sketch a `build_verifier_jwks()` that returns a
   `JwksVerifier::new("https://idp.example.com/.well-known/jwks.json")` and
   confirm (by reading the `Verifier` trait) that `security_layers` needs no
   other change. Why does the rest of Lumen not care which verifier it got?
4. **Expiry.** Lower the token lifetime with
   `JwtService::new(KEY).expiration_seconds(1)`, mint a token, wait two seconds,
   and assert the verifier now returns `SecurityError::Verification`.

A secure service is only trustworthy if you can *see* what it is doing. The next
chapter gives Lumen eyes and ears — structured logs, health, metrics, and the
admin dashboard. Continue to [Observability](./15-observability.md).
