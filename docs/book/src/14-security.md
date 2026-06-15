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

> **What's in the box.** `firefly-security` is a complete resource-server
> stack: a `BearerLayer` + `Verifier` for token validation, a URL-based
> authorization `FilterChain`, method guards, JWKS validation, an OAuth2 client +
> authorization server, a role hierarchy, CSRF, and a bcrypt encoder. Auth
> failures render as RFC 9457 `application/problem+json` on a 401/403 — a stable,
> standards-based wire contract that off-the-shelf clients and gateways
> understand.

## The mental model

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 470 384" role="img"
     aria-label="The security request pipeline: incoming request flows through the BearerLayer, then the RBAC FilterChain, then reaches the WalletApi handlers"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <!-- incoming request -->
  <text x="235" y="20" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">incoming request</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="235" y1="28" x2="235" y2="46"/><polygon points="235,54 231,46 239,46"/>
  </g>
  <!-- BearerLayer -->
  <rect x="55" y="56" width="360" height="104" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <path d="M55,66 a10,10 0 0 1 10,-10 h340 a10,10 0 0 1 10,10 v16 h-360 z" fill="#f6a821"/>
  <text x="235" y="77" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10">BearerLayer</text>
  <g font-size="11" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="72" y="100">reads Authorization: Bearer &lt;tok&gt;</text>
    <text x="72" y="116">calls the Verifier (JwtService)</text>
    <text x="72" y="132">stores Authentication on request</text>
    <text x="72" y="148">allow_anonymous &#8594; pass empty ctx</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="235" y1="160" x2="235" y2="178"/><polygon points="235,186 231,178 239,178"/>
  </g>
  <!-- FilterChain (RBAC) -->
  <rect x="55" y="188" width="360" height="104" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <path d="M55,198 a10,10 0 0 1 10,-10 h340 a10,10 0 0 1 10,10 v16 h-360 z" fill="#f6a821"/>
  <text x="235" y="209" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10">FilterChain (RBAC)</text>
  <g font-size="11" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="72" y="232">permit_method("GET", "/api/v1/...")</text>
    <text x="72" y="248">permit("/actuator/")</text>
    <text x="72" y="264">require("/api/v1/wallets", CUSTOMER)</text>
    <text x="72" y="280">401 / 403 problem+json on a miss</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="235" y1="292" x2="235" y2="310"/><polygon points="235,318 231,310 239,310"/>
  </g>
  <!-- WalletApi handlers -->
  <rect x="125" y="320" width="220" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="235" y="344" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c"
        font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">WalletApi handlers</text>
</svg>
<figcaption>The security request pipeline. Every request passes the <code>BearerLayer</code> (authentication) and the RBAC <code>FilterChain</code> (authorization) before reaching the <code>WalletApi</code> handlers.</figcaption>
</figure>

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
> request matching no rule is rejected with 403 (deny-by-default).
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

## Method security

The `FilterChain` guards *routes*. But authorization is often a property of a
*service method* — a domain operation that several handlers, a scheduled job, and
a CQRS handler all call. Pushing the check down to the method means it holds no
matter how the operation is reached, and the route table stays about routes.

Firefly does this with two attribute macros and an **ambient security context**.
The macros declare the rule; the context carries the caller's `Authentication`
through the call stack so the method never has to thread an argument or touch the
`Request`.

### `#[firefly::pre_authorize(...)]`

`#[firefly::pre_authorize(...)]` guards a function *before* its body runs. It
attaches to any function returning `Result<T, E>` where `E:
From<firefly_security::SecurityError>`, and reads the ambient `Authentication`
(below) to decide. The rules:

| Rule                          | Passes when                                         |
|-------------------------------|-----------------------------------------------------|
| *(empty)* / `authenticated`   | a caller is in scope (the default)                  |
| `role = "ADMIN"`              | the caller has role `ADMIN`                         |
| `any_role = ["A", "B"]`       | the caller has *any* of the listed roles            |
| `authority = "wallet:write"`  | the caller holds that authority (role or scope)     |
| `any_authority = ["a", "b"]`  | the caller holds *any* of the listed authorities    |

On denial the macro returns early with `Err(..)`: an **`Unauthenticated`**
`SecurityError` when no caller is in scope, a **`Forbidden`** one when a caller is
present but the authorities don't match.

```rust,ignore
use firefly_security::SecurityError;

/// Only a CUSTOMER may withdraw. The check runs before any balance logic.
#[firefly::pre_authorize(role = "CUSTOMER")]
pub async fn withdraw(wallet: WalletId, amount: Money) -> Result<Wallet, WalletError>
where
    WalletError: From<SecurityError>,
{
    // ... domain logic; reached only for an authenticated CUSTOMER ...
}

/// A coarse "must be logged in" gate — the empty form is `authenticated`.
#[firefly::pre_authorize]
pub fn current_balance(wallet: WalletId) -> Result<Money, WalletError> {
    // ...
}

/// A fine-grained scope check rather than a role.
#[firefly::pre_authorize(authority = "wallet:approve")]
pub async fn approve(wallet: WalletId) -> Result<(), WalletError> {
    // ...
}
```

### `#[firefly::post_authorize(<bool expr>)]`

Sometimes you can only decide *after* you have the value — "you may read this
wallet only if you own it." `#[firefly::post_authorize(...)]` attaches to an
`async fn` returning `Result<T, E>` and evaluates a boolean expression once the
body has produced its `Ok(T)`. The expression sees two bindings:

- `result` — a `&T`, the value the function is about to return (the *return
  object*).
- `auth` — a `&Authentication`, the ambient caller.

If the expression is `false` the value is **discarded** and the call resolves to
a `Forbidden` error instead:

```rust,ignore
/// A caller may fetch a wallet only if they own it.
#[firefly::post_authorize(result.owner == auth.principal)]
pub async fn get_wallet(id: WalletId) -> Result<Wallet, WalletError> {
    repo().load(id).await // produces Ok(Wallet); the rule then vets the owner
}
```

### The ambient context — `firefly_security`

Both macros read an ambient `Authentication` rather than an argument. That scope
is managed by a small set of functions in `firefly_security` — the security
context that travels with the task:

```rust,ignore
use firefly_security::{
    with_authentication_scope, current_authentication, check_access,
    AccessRule, Authentication, SecurityError,
};

// Run `fut` with `auth` installed as the ambient caller for its whole duration.
let wallet = with_authentication_scope(auth, async {
    withdraw(id, amount).await // #[pre_authorize] inside sees `auth`
}).await?;

// Read the current caller anywhere downstream (None if unauthenticated).
let who: Option<Authentication> = current_authentication();

// Imperative check when a macro doesn't fit — returns the Authentication on
// success, a SecurityError on failure.
let auth: Authentication = check_access(&AccessRule::Role("CUSTOMER"))?;
```

`AccessRule` is the runtime form of the macro rules:
`AccessRule::Authenticated`, `Role(&str)`, `AnyRole(&[&str])`, `Authority(&str)`,
and `AnyAuthority(&[&str])`.

The payoff is that **`BearerLayer` installs the scope for you**. On every request
— both the verified path *and* the anonymous (`allow_anonymous`) path — the
bearer layer wraps the downstream call in `with_authentication_scope`, so a
service method decorated with `#[pre_authorize]` works correctly even though it
never sees the `Request`. URL rules and method rules then compose: the
`FilterChain` is your coarse perimeter, the method macros are your defense in
depth.

## Wiring security from configuration

Lumen inlines its signing key and builds the bearer layer by hand because that
makes the sample runnable as-is. A deployed service instead reads its security
posture from configuration, and `firefly_security` binds it directly — no DI
container, no framework callback. The properties live under `firefly.security.*`
and bind through `serde`:

```rust,ignore
use firefly_security::{
    SecurityProperties, JwtProperties, BearerProperties,
    verifier_from_config, bearer_layer_from_config,
};
```

```toml
# firefly.security.* — JWKS resource-server example
[firefly.security.jwt]
jwk_set_uri = "https://idp.example.com/.well-known/jwks.json"
issuer_uri  = "https://idp.example.com/"
audience    = "lumen"
algorithm   = "RS256"

[firefly.security.bearer]
header_name     = "Authorization"
allow_anonymous = true
```

The structs mirror that shape:

```rust,ignore
pub struct SecurityProperties {
    pub jwt: JwtProperties,
    pub bearer: BearerProperties,
}

// Both structs derive `Default, Deserialize` with `#[serde(default)]`, so a
// missing field falls back to its zero value (empty `String`, `0`).
pub struct JwtProperties {
    pub jwk_set_uri: String,
    pub issuer_uri: String,
    pub audience: String,
    pub secret: String,
    pub algorithm: String,
    pub expiration_seconds: u64,
}

pub struct BearerProperties {
    pub header_name: String,
    pub allow_anonymous: bool,
}
```

Two builder functions turn bound properties into ready components:

```rust,ignore
use std::sync::Arc;
use firefly_security::{Verifier, BearerLayer, SecurityError};

// Pick a verifier by what configuration provides — JWKS first, then HMAC,
// then nothing.
let verifier: Option<Arc<dyn Verifier>> = verifier_from_config(&props.jwt)?;

// The fully-assembled bearer layer (header name + anonymous policy applied),
// or None when no verifier is configured.
let bearer: Option<BearerLayer> = bearer_layer_from_config(&props)?;
```

`verifier_from_config(&JwtProperties)` resolves the verifier by precedence: a
non-empty `jwk_set_uri` builds a JWKS (RS256) resource-server verifier; otherwise
a non-empty `secret` builds an HMAC (HS256/384/512) verifier; otherwise it
returns `None`. `bearer_layer_from_config(&SecurityProperties)` builds the
verifier the same way and, if there is one, wraps it in a `BearerLayer` with the
configured header name and anonymous policy already applied — the same layer
`security_layers` builds by hand, sourced from config instead. Switching Lumen
from the demo HMAC key to a production IdP becomes a configuration change, with
no edit to `security.rs`.

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
- **Method guards.** For per-handler checks, the `guards` module composes typed
  predicates: `has_role("CUSTOMER").or(has_authority("wallet:approve"))`, then
  `guard.authorize(Some(&auth))?` — 401 with no principal, 403 if the predicate
  is false. For a declarative spelling, see [Method security](#method-security)
  above.
- **Role hierarchy.** `RoleHierarchy::from_string("ADMIN > CUSTOMER")` parses the
  spec; attach it with `chain.with_role_hierarchy(..)` so granting `ADMIN` implies
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
use firefly_security::{BcryptPasswordEncoder, PasswordEncoder};

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
- **Method security** pushes authorization onto domain operations:
  `#[firefly::pre_authorize(role = "CUSTOMER")]` guards before the body runs,
  `#[firefly::post_authorize(result.owner == auth.principal)]` vets the return
  object, and the `BearerLayer` installs the ambient context
  (`with_authentication_scope` / `current_authentication` / `check_access`) so a
  service method enforces the rule without ever seeing the `Request`.
- **Config-driven wiring** lets a deployed service read its posture from
  `firefly.security.*`: `verifier_from_config` picks JWKS (RS256), HMAC, or none
  by precedence, and `bearer_layer_from_config` hands back the ready-to-mount
  `BearerLayer` — moving from the demo key to a production IdP with no code edit.
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
