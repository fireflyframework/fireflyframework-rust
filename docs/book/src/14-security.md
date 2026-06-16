# Security

In [HTTP Clients](./13-http-clients.md) you saw how Lumen *would* call an
external payments or FX provider. Lumen itself, though, is still wide open: any
caller can open a wallet, deposit, withdraw, or move money between wallets.
Before Part V can ship Lumen to production, you have to close that door — and
you will do it without adding a dependency, hand-rolling crypto, or rewriting a
single handler.

By the end of this chapter Lumen will **authenticate** every request with a
signed JWT, **authorize** the mutating routes with a path-based RBAC filter
chain, and leave the public reads and the management surface open. The whole
thing is built on the framework's security tier, reached through the one
`firefly` facade you have depended on since [Quickstart](./02-quickstart.md).

By the end of this chapter you will:

- Mint and verify signed HS256 tokens with `JwtService`, and understand why
  every issued token carries a bounded `exp`.
- Adapt that service into a `Verifier` and turn a request's bearer token into an
  `Authentication` (principal, roles, claims).
- Compose a `BearerLayer` and a path-ordered RBAC `FilterChain`, and understand
  fail-closed (deny-by-default) ordering.
- Wire both as `#[bean]`s and watch `FireflyApplication` auto-discover and layer
  them — no `.with_security(...)` call anywhere.
- Push authorization down to a service method with `#[firefly::pre_authorize]` /
  `#[firefly::post_authorize]` over an ambient security context.
- Move the same posture into configuration so production swaps the demo key for
  a real IdP with no code edit.

## Concepts you will meet

Before the first line of code, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — authentication vs. authorization.** *Authentication*
> answers "who is this caller?" — it validates a credential and resolves a
> principal. *Authorization* answers "may they do this?" — it checks that the
> resolved principal is allowed to perform the operation. They are two distinct
> stages, and Firefly keeps them in two distinct components.

> **Note** **Key term — JWT (JSON Web Token).** A *JWT* is a compact,
> URL-safe, signed token carrying a JSON payload of *claims* (`sub`, `roles`,
> `exp`, …). Because the signature proves the payload was not tampered with, a
> stateless service can trust a token's claims without a server-side session or
> a round-trip to a database. The Spring analog is a Spring Security resource
> server validating a `Bearer` token.

> **Note** **Key term — RBAC (Role-Based Access Control).** *RBAC* grants
> access by the *roles* a caller holds (here `CUSTOMER`) rather than by their
> identity. A rule says "this route requires role X"; a caller passes if their
> token carries X. This is the model Spring Security's URL authorization rules
> use.

> **Note** **Key term — resource server.** A *resource server* is a service
> that protects its own endpoints by validating an access token minted
> elsewhere (an identity provider). It never logs anyone in; it only *verifies*
> the credential it is handed. Lumen is a resource server — in the demo it also
> mints its own tokens for testing, but the verification path is identical to a
> production IdP.

The framework's security tier carries far more than Lumen uses — JWKS, OAuth2
(client + authorization server), role hierarchy, method guards, CSRF, and
password encoders. We tour those at the end; first, the four pieces Lumen
actually wires, in the order a request meets them.

## The request pipeline at a glance

Every request travels through two security stages before it reaches a handler:

```text
            incoming request
                   │
                   ▼
   ┌───────────────────────────────────────┐
   │              BearerLayer               │  (authentication)
   │  • reads Authorization: Bearer <tok>   │
   │  • calls the Verifier (JwtService)     │
   │  • stores Authentication on the request│
   │  • allow_anonymous → pass empty ctx    │
   └───────────────────────────────────────┘
                   │
                   ▼
   ┌───────────────────────────────────────┐
   │           FilterChain (RBAC)           │  (authorization)
   │  permit_method("GET", "/api/v1/...")   │
   │  permit("/actuator/")                  │
   │  require("/api/v1/wallets", CUSTOMER)  │
   │  401 / 403 problem+json on a miss      │
   └───────────────────────────────────────┘
                   │
                   ▼
            WalletApi handlers
```

The `BearerLayer` authenticates (who?), the `FilterChain` authorizes (may
they?), and only a request that clears both reaches Lumen's handlers. Auth
failures render as RFC 9457 `application/problem+json` on a 401 or 403 — a
stable, standards-based wire contract that off-the-shelf clients and gateways
already understand.

> **Note** **Key term — RFC 9457 problem+json.** RFC 9457 (which obsoletes RFC
> 7807) standardizes machine-readable error bodies under the
> `application/problem+json` media type — a JSON object with `type`, `title`,
> `status`, and `detail` members. Firefly renders every security rejection this
> way, so a 401 or 403 is a structured document, not a blank body. You met this
> renderer in [Your First HTTP API](./06-first-http-api.md); here it carries the
> auth failures too.

## Step 1 — Mint and verify tokens with `JwtService`

Lumen is a stateless API. Sessions would need sticky routing or a shared store
on every replica; a signed JWT lets each request carry its own credential, so
the service scales horizontally with no shared state. The framework's
`JwtService` both **mints** the demo tokens and **verifies** the incoming ones,
using a symmetric HS256 key — which is exactly what makes Lumen runnable and
testable with no external IdP.

> **Note** **Key term — symmetric (HS256) signing.** HS256 signs and verifies
> with the *same* shared secret. That is the simplest scheme to run — one key,
> no key server — and the right fit for a self-contained sample. (A production
> deployment usually moves to *asymmetric* RS256, where an IdP signs with a
> private key and your service verifies with the matching public key fetched
> from a JWKS endpoint. Step 7 shows that swap.)

Create `src/security.rs` and start with the signing key, the role constant, and
the shared service:

```rust,ignore
// src/security.rs
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

What just happened, block by block:

- The `use` line pulls the entire token surface from `firefly::security` — the
  facade re-exports the framework's security crate, so there is no new
  dependency to add to `Cargo.toml`.
- `JwtService::new(secret)` builds an HS256 service over the secret. Construction
  takes anything `AsRef<[u8]>`, so the inline byte-string key works directly.
- `encode` signs a JSON payload and — this is the load-bearing detail —
  **injects an `exp` claim** when the payload has none, defaulting to one hour
  out (`DEFAULT_EXPIRATION_SECONDS = 3600`). Every token Lumen issues therefore
  has a bounded lifetime. A token minted *without* `exp` (one that would never
  expire) is rejected at decode time, because `decode` lists `exp` as a required
  claim.
- `mint_token` is the helper the HTTP tests call to obtain a credential the
  verifier will accept. The `.expect(...)` is safe here: signing a fixed,
  well-formed claim shape cannot fail.

> **Tip** **Checkpoint.** A quick mental dry run: `mint_token("u-alice",
> &["CUSTOMER"])` returns a three-segment `header.payload.signature` string.
> Decoding its middle segment (base64) would show `sub`, `roles`, and an
> auto-stamped `exp` roughly one hour in the future.

## Step 2 — Turn the service into a `Verifier`

`JwtService` can already verify — it implements the `Verifier` trait directly.
But Lumen wraps it in a small adapter so the *error shape* is exactly what the
`BearerLayer` wants to render.

> **Note** **Key term — `Verifier` (the authentication port).** A `Verifier` is
> the resource-server *port*: given a raw token, validate it and return an
> `Authentication` (the principal, username, roles, and raw claims), or a
> `SecurityError` on failure. It is a trait, so any token validator — the demo
> HS256 service, a JWKS verifier, your own closure — satisfies the same
> contract. `VerifierFn` adapts a plain async closure into one.

Add the verifier builder:

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

What just happened: `VerifierFn(closure)` wraps a plain `async` closure as a
`Verifier`. The closure delegates to `JwtService::to_authentication`, which
decodes the token and maps its claims onto an `Authentication` — `sub` becomes
the principal, the `roles` array becomes roles, and every decoded claim is kept.
Any failure (bad signature, expired, missing `exp`) is re-wrapped as
`SecurityError::Verification(..)`; the `BearerLayer` turns that into the canonical
401 problem.

> **Note** **Key term — `Authentication`.** `Authentication` is the resolved
> caller the rest of the stack inspects. It is the Rust analog of Spring
> Security's `Authentication` object. Its fields:
>
> | Field       | Type                                  | From the claim                |
> |-------------|---------------------------------------|-------------------------------|
> | `principal` | `String`                              | `sub`                         |
> | `username`  | `String`                              | `preferred_username` / `name`, else `sub` |
> | `roles`     | `Vec<String>`                         | `roles`                       |
> | `authorities` | `Vec<String>`                       | `permissions` (and OAuth2 scopes) |
> | `claims`    | `HashMap<String, serde_json::Value>`  | every decoded claim           |
>
> Its helpers cover the common checks: `has_role(r)`,
> `has_any_role(&[..])`, `has_authority(a)` (matches a role *or* a fine-grained
> permission/scope), `has_any_authority(&[..])`, and the
> `Authentication::anonymous()` constructor.

A unit test asserts the round-trip directly — mint a token, verify it, confirm
the principal and role survived:

```rust,ignore
#[tokio::test]
async fn mint_then_verify_roundtrips_claims() {
    use firefly::security::Authentication;
    let token = mint_token("u-alice", &[CUSTOMER_ROLE]);
    let auth: Authentication = build_verifier().verify(&token).await.unwrap();
    assert_eq!(auth.principal, "u-alice");
    assert!(auth.has_role(CUSTOMER_ROLE));
}
```

A tampered token (`"not.a.jwt"`) or one signed with the wrong key is rejected
with `SecurityError::Verification` — two negative tests in `security.rs` prove
it:

```rust,ignore
#[tokio::test]
async fn tampered_token_is_rejected() {
    let err = build_verifier().verify("not.a.jwt").await.unwrap_err();
    assert!(matches!(err, SecurityError::Verification(_)));
}
```

> **Tip** **Checkpoint.** Run `cargo test mint_then_verify` (or the whole
> `security` module). The round-trip test passes, and the two rejection tests
> confirm a bad credential never resolves to an `Authentication`. Authentication
> now works end to end, before any HTTP wiring.

## Step 3 — Compose the `BearerLayer` and the RBAC `FilterChain`

`JwtService` answers *who is this caller?*; the `FilterChain` answers *may they
do this?* The chain matches request paths against rules in declaration order —
**first match wins** — and renders a 401 (no/invalid credential) or 403
(authenticated but under-privileged). Lumen composes the bearer layer and the
chain in one function.

> **Note** **Key term — `BearerLayer`.** The `BearerLayer` is the tower
> middleware that performs authentication on the wire: it reads the
> `Authorization: Bearer <token>` header, calls the `Verifier`, and stores the
> resulting `Authentication` on the request before the chain runs. It is the
> Rust analog of Spring Security's bearer-token authentication filter.

> **Note** **Key term — `FilterChain`.** The `FilterChain` is the path-based
> authorization matcher — the Rust analog of Spring Security's URL authorization
> rules (`authorizeHttpRequests`). You build it with `permit` / `require` /
> `permit_method` calls; each adds one ordered rule.

Add the composition function:

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

Two design choices are worth dwelling on, because they decide who gets a 401 vs.
who slips through:

- **`allow_anonymous(true)` on the bearer layer.** With it set, a request with
  no `Authorization` header is *not* rejected at the bearer layer — it reaches
  the chain carrying an anonymous `Authentication`. That keeps a single
  decision-maker: the `FilterChain` decides every route. A public `GET` passes;
  a `require` route with no valid token becomes a 401. Without `allow_anonymous`,
  the bearer layer would reject anonymous traffic *before* the chain could permit
  the public reads — so the public wallet read would break.
- **Order matters.** `permit_method("GET", "/api/v1/wallets")` and
  `permit("/actuator/")` come *first*, so the public reads and the management
  surface are decided before the broad `require("/api/v1/wallets", ...)` could
  catch them. First match wins, so a more specific permit must precede a broader
  require. `any_request_permit()` then re-opens the unmatched tail (see the
  warning below).

> **Warning** Once any rule is declared, a `FilterChain` is **fail-closed**: a
> request matching no rule is rejected with 403 (deny-by-default, matching Spring
> Security 6). Re-open the unmatched tail explicitly with `any_request_permit()`
> / `any_request_authenticated()` / `any_request_deny()`. A chain with *no* rules
> at all is a no-op and passes everything — so an empty chain is never a surprise
> blanket lockout, but the moment you add your first rule, everything you did
> not name is denied unless a catch-all re-opens it.

> **Tip** **Checkpoint.** Trace each route through the rule list by hand: `GET
> /api/v1/wallets/w-1` hits the first `permit_method` and passes; `GET
> /actuator/health` hits `permit("/actuator/")` and passes; `POST
> /api/v1/wallets` falls past both permits to `require("/api/v1/wallets",
> [CUSTOMER])`; an unmatched path like `GET /favicon.ico` reaches
> `any_request_permit()` and passes. If you mentally reordered the requires above
> the permits, the public read would now demand a token — that is the "first
> match wins" trap in action.

## Step 4 — Wire the layers as beans

Lumen does **not** layer security by hand. The `FilterChain` and the
`BearerLayer` are each declared as a `#[bean]` in `LumenBeans` — the
`#[derive(Configuration)]` holder in `src/web.rs` you have been growing since the
DI chapter — and `FireflyApplication` auto-discovers and applies them. This is
the Rust analog of Spring's `SecurityFilterChain` bean: declaring the bean *is*
the wiring.

> **Note** **Key term — security as discovered beans.** In Spring Boot you
> register a `SecurityFilterChain` `@Bean` and the framework applies it; you
> never call a `with_security(...)` method. Firefly works the same way: a
> `FilterChain` bean and a `BearerLayer` bean are auto-discovered at boot and
> layered onto the router. There is no `.with_security(...)` call and no manual
> `.layer(bearer)` in app code.

Add the two bean methods to the existing `#[bean] impl LumenBeans` block:

```rust,ignore
// samples/lumen/src/web.rs — inside #[bean] impl LumenBeans { ... }
use firefly::security::{BearerLayer, FilterChain};

/// The HTTP security filter chain (path-based RBAC) — the Spring
/// `SecurityFilterChain` bean. `FireflyApplication` auto-discovers + applies it.
#[bean]
fn security_filter_chain(&self) -> FilterChain {
    crate::security::security_layers().1
}

/// The bearer-token authentication layer — auto-discovered + layered onto
/// the API by `FireflyApplication`.
#[bean]
fn bearer_layer(&self) -> BearerLayer {
    crate::security::security_layers().0
}
```

What just happened, and what the framework does with it at boot:

- Each `#[bean]` method declares one component for the container to construct.
  `security_layers()` returns the `(BearerLayer, FilterChain)` tuple; one bean
  hands back `.0`, the other `.1`.
- At startup `run()` resolves the `FilterChain` bean and sets it on the web
  stack, then resolves the `BearerLayer` bean and layers it around the whole
  router so the chain always sees a populated `Authentication`.
- The chain runs *inside* the inherited correlation / security-headers / CORS
  edge, so even a 401 response carries those headers and a correlation id. The
  bearer layer goes on the *outside* — axum runs the last-added layer first, so
  the order is **authenticate, then authorize**.

Declaring the two beans is the *entire* wiring: no `with_security`, no
`apply_middleware` call, no edit to `main`. This is the "no `main` churn"
property from [Quickstart](./02-quickstart.md) at work — security is just more
beans for the framework to discover.

> **Tip** **Checkpoint.** Run `cargo run` and read the startup report's
> `:: beans ::` line — `security_filter_chain` and `bearer_layer` now appear in
> the discovered-bean inventory. Then `curl -i -X POST localhost:8080/api/v1/wallets
> -H 'content-type: application/json' -d '{"owner":"mallory","openingBalance":10}'`:
> you get a `401` with `content-type: application/problem+json`, because the
> mutation now requires a `CUSTOMER` token. The public read still works:
> `curl localhost:8080/api/v1/wallets/anything` is no longer rejected for lack of
> a token (it 404s on an unknown id, which is a different, business-level
> outcome).

## Step 5 — Push authorization down to a method

The `FilterChain` guards *routes*. But authorization is often a property of a
*service method* — a domain operation that several handlers, a scheduled job,
and a CQRS handler all call. Pushing the check down to the method means it holds
no matter how the operation is reached, and the route table stays about routes.

Firefly does this with two attribute macros and an **ambient security context**.
The macros declare the rule; the context carries the caller's `Authentication`
through the call stack so the method never has to thread an argument or touch the
`Request`.

> **Note** **Key term — ambient security context.** The *ambient context* is a
> task-local slot holding the current `Authentication` — the Rust analog of
> Spring's `SecurityContextHolder` and its thread-local. The `BearerLayer`
> installs it for the duration of each request, so any method reached downstream
> can read the caller without it riding every function signature. Because the
> slot is task-local it nests cleanly and never leaks across spawned tasks.

### `#[firefly::pre_authorize(...)]`

`#[firefly::pre_authorize(...)]` guards a function *before* its body runs. It
attaches to any function returning `Result<T, E>` where `E:
From<firefly_security::SecurityError>`, and reads the ambient `Authentication` to
decide. The rules:

| Rule                          | Passes when                                         |
|-------------------------------|-----------------------------------------------------|
| *(empty)* / `authenticated`   | a real (non-anonymous) caller is in scope (default) |
| `role = "ADMIN"`              | the caller has role `ADMIN`                         |
| `any_role = ["A", "B"]`       | the caller has *any* of the listed roles            |
| `authority = "wallet:write"`  | the caller holds that authority (role or scope)     |
| `any_authority = ["a", "b"]`  | the caller holds *any* of the listed authorities    |

On denial the macro returns early with `Err(..)`: an `Unauthenticated`
`SecurityError` when no caller is in scope, a `Forbidden` one when a caller is
present but the authorities don't match. The `?` inside the generated code
propagates that error through your `From<SecurityError>` impl.

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

- `result` — a `&T`, the value the function is about to return (Spring's
  *return object*).
- `auth` — a `&Authentication`, the ambient caller.

If the expression is `false` the value is **discarded** and the call resolves to
a `Forbidden` error instead; if no context is active at all it resolves to
`Unauthenticated`:

```rust,ignore
/// A caller may fetch a wallet only if they own it.
#[firefly::post_authorize(result.owner == auth.principal)]
pub async fn get_wallet(id: WalletId) -> Result<Wallet, WalletError> {
    repo().load(id).await // produces Ok(Wallet); the rule then vets the owner
}
```

### The ambient context functions

Both macros read the ambient `Authentication` rather than an argument. That scope
is managed by a small set of functions in `firefly_security`, reached through the
facade as `firefly::security`:

```rust,ignore
use firefly::security::{
    with_authentication_scope, current_authentication, check_access,
    AccessRule, Authentication, SecurityError,
};

// Run `fut` with `auth` installed as the ambient caller for its whole duration.
let wallet = with_authentication_scope(auth, async {
    withdraw(id, amount).await // #[pre_authorize] inside sees `auth`
}).await?;

// Read the current caller anywhere downstream (None if no scope is active).
let who: Option<Authentication> = current_authentication();

// Imperative check when a macro doesn't fit — returns the Authentication on
// success, a SecurityError on failure.
let auth: Authentication = check_access(&AccessRule::Role("CUSTOMER"))?;
```

`AccessRule` is the runtime form of the macro rules: `AccessRule::Authenticated`,
`Role(&str)`, `AnyRole(&[&str])`, `Authority(&str)`, and `AnyAuthority(&[&str])`.

The payoff is that **`BearerLayer` installs the scope for you**. On every request
— both the verified path *and* the anonymous (`allow_anonymous`) path — the
bearer layer wraps the downstream call in `with_authentication_scope`, so a
service method decorated with `#[pre_authorize]` works correctly even though it
never sees the `Request`. URL rules and method rules then compose: the
`FilterChain` is your coarse perimeter, the method macros are your defense in
depth.

> **Tip** **Checkpoint.** The key invariant to hold in your head: a
> `#[pre_authorize]` method called *outside* any scope (e.g. directly from a
> plain `#[test]` without `with_authentication_scope_sync`) returns
> `Unauthenticated` — the macro fails closed when there is no caller, exactly
> like the route chain.

## Step 6 — Prove it end to end over HTTP

The HTTP suite (`tests/http.rs`, in the sample at `src/http_test.rs`) drives the
fully-wired router with `tower::ServiceExt::oneshot` and asserts the security
behavior directly — no socket bound. The router comes from `build_router`, which
boots the same app `main()` does:

```rust,ignore
// The testable in-process public router — every bean (including the
// FilterChain + BearerLayer) is auto-discovered, exactly as in `main`.
#[cfg(test)]
pub(crate) async fn build_router() -> axum::Router {
    firefly::FireflyApplication::new(APP_NAME)
        .version(VERSION)
        .bootstrap()
        .await
        .expect("lumen bootstrap")
        .api_router
}
```

> **Note** **Testing seam.** `bootstrap()` is the sibling of `run()` from
> [Quickstart](./02-quickstart.md): it assembles the same app — security beans
> included — but returns a `Bootstrapped` value *without* serving, so a test can
> drive the wired public router (`Bootstrapped::api_router`) in-process. You met
> this in [Your First HTTP API](./06-first-http-api.md); here it lets the suite
> exercise the real `BearerLayer` + `FilterChain`.

A request helper builds the `Authorization` header from `mint_token`, so an
authenticated request is just `post(path, body, true)` and an unauthenticated one
is `post(path, body, false)`:

```rust,ignore
fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &[CUSTOMER_ROLE]))
}

fn post(path: &str, body: serde_json::Value, auth: bool) -> Request<Body> {
    let mut b = Request::post(path).header("content-type", "application/json");
    if auth {
        b = b.header("authorization", bearer());
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()
}
```

A mutation with **no** token is a 401 problem:

```rust,ignore
#[tokio::test]
async fn missing_token_is_401_problem_on_mutations() {
    let app = build_router().await;
    let res = send(
        &app,
        post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "mallory", "openingBalance": 10 }),
            false, // no Authorization header
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(content_type(&res).contains("application/problem+json"));
}
```

Every other test in the suite — open, get, deposit/withdraw, transfer — runs as
the minted `CUSTOMER` (it passes `true`), so authentication is exercised on the
happy path too. A 401 proves the perimeter is closed; the green happy-path tests
prove a valid token still gets through.

> **Tip** **Checkpoint.** Run `cargo test` for Lumen. The `missing_token_is_401`
> test is red-then-green proof the front door is shut, and the wallet round-trip
> tests confirm a `CUSTOMER` token still opens it. If the 401 test fails with a
> `201 Created`, the `FilterChain` bean is not being discovered — confirm both
> `#[bean]` methods compile inside the `LumenBeans` block.

## Step 7 — Move the posture into configuration

Lumen inlines its signing key and builds the bearer layer by hand because that
makes the sample runnable as-is. A deployed service instead reads its security
posture from configuration, and `firefly_security` binds it directly — no DI
container, no framework callback. The properties live under `firefly.security.*`
and bind through `serde`:

```rust,ignore
use firefly::security::{
    SecurityProperties, JwtProperties, BearerProperties,
    verifier_from_config, bearer_layer_from_config,
};
```

A JWKS resource-server posture in `firefly.yaml`:

```yaml
firefly:
  security:
    jwt:
      jwk-set-uri: "https://idp.example.com/.well-known/jwks.json"
      issuer-uri: "https://idp.example.com/"
      audience: "lumen"
      algorithm: "RS256"
    bearer:
      header-name: "Authorization"
      allow-anonymous: true
```

The structs mirror that shape; each derives `Default` + `Deserialize` with
`#[serde(default)]`, so a missing field falls back to its zero value:

```rust,ignore
pub struct SecurityProperties {
    pub jwt: JwtProperties,
    pub bearer: BearerProperties,
}

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
use firefly::security::{Verifier, BearerLayer, SecurityError};

// Pick a verifier by what configuration provides — JWKS first, then HMAC,
// then nothing.
let verifier: Option<Arc<dyn Verifier>> = verifier_from_config(&props.jwt)?;

// The fully-assembled bearer layer (header name + anonymous policy applied),
// or None when no verifier is configured.
let bearer: Option<BearerLayer> = bearer_layer_from_config(&props)?;
```

What just happened: `verifier_from_config(&JwtProperties)` resolves the verifier
by precedence — a non-empty `jwk_set_uri` builds a JWKS (RS256) resource-server
verifier; otherwise a non-empty `secret` builds an HMAC (HS256/384/512) verifier;
otherwise it returns `None`. `bearer_layer_from_config(&SecurityProperties)`
builds the verifier the same way and, if there is one, wraps it in a
`BearerLayer` with the configured header name and anonymous policy already
applied — the same layer `security_layers` builds by hand, sourced from config
instead. Switching Lumen from the demo HMAC key to a production IdP becomes a
configuration change, with no edit to `security.rs`.

> **Note** **Key term — JWKS (JSON Web Key Set).** A *JWKS* is the public-key
> set an identity provider publishes at a well-known URL. A resource server
> fetches it to verify RS256 tokens, keying on each token's `kid` (key id) and
> caching the result. `JwksVerifier` is the framework's drop-in `Verifier` for
> this: same `Verifier` port, so `security_layers` — and every handler — is
> untouched when you swap the demo HMAC verifier for it. That is the "swap the
> adapter, keep the code" promise applied to identity.

## The rest of the security tier — Lumen's growth room

Lumen uses the symmetric-key fast path. The same crate carries the production
surface you reach for as a real wallet service matures:

- **JWKS verification.** `JwksVerifier::new("https://idp.example.com/.well-known/jwks.json")`
  is a drop-in `Verifier` for RS256 tokens from an external IdP (Keycloak,
  Auth0, Cognito): `kid` cache, `iss`/`aud` checks via `.issuer(..)` /
  `.audience(..)`, `exp` required, and the same `sub`/`roles`/`permissions` claim
  mapping.
- **Method guards.** For imperative per-handler checks, the `guards` module
  composes typed predicates: `guards::has_role("CUSTOMER").or(guards::has_authority("wallet:approve"))`,
  then `guard.authorize(Some(&auth))?` — `Unauthenticated` with no principal,
  `Forbidden` if the predicate is false. For a declarative spelling, prefer the
  [method-security macros](#step-5--push-authorization-down-to-a-method) above.
- **Role hierarchy.** `RoleHierarchy::from_string("ADMIN > CUSTOMER")` parses the
  spec; attach it with `chain.with_role_hierarchy(..)` so granting `ADMIN`
  implies `CUSTOMER` everywhere the chain checks a role.
- **Pattern rules.** Alongside the prefix rules Lumen uses, the chain offers an
  fnmatch-style glob DSL — `permit_pattern("/public/**")`,
  `require_pattern("/api/admin/**", &["ADMIN"])`,
  `require_authority("/api/reports/**", &["reports:read"])`, and
  `authenticated("/api/**")`.
- **Sessions.** For browser flows where logout must mean logout, the
  `firefly-session` crate adds a `SessionLayer` over a `SessionStore`
  (`MemorySessionStore` for dev, a Redis-backed store for scale). A handler pulls
  the request's `Session` with the `SessionExt` extractor and calls
  `session.rotate_id().await` after login (session-fixation defense),
  `session.set_attribute("user_id", &id).await`, and `session.invalidate().await`
  on logout.
- **OAuth2.** The `oauth2` module covers both sides: `ClientRegistration` (with
  `google` / `github` / `keycloak` presets) + `OAuth2LoginHandler` for the
  authorization-code login flow (state + nonce + PKCE S256, OIDC id-token
  validation), and an `AuthorizationServer` that issues tokens for
  `client_credentials` / `refresh_token`.
- **CSRF & passwords.** `CsrfLayer` implements the double-submit-cookie pattern
  for cookie-session flows; `BcryptPasswordEncoder` (default work factor 12)
  hashes credentials, and `Argon2PasswordEncoder` (Argon2id, OWASP defaults via
  `new()` — `m=19456` KiB, `t=2`, `p=1` — or `with_params(m, t, p)`) is the
  memory-hard alternative behind the *same* `PasswordEncoder` port. The `$2b$`
  bcrypt hashes and the self-describing `$argon2id$` PHC strings both interchange
  with the `firefly-idp-internal-db` adapter and every other port.

Both encoders share one trait, so they are interchangeable:

```rust
use firefly_security::{Argon2PasswordEncoder, BcryptPasswordEncoder, PasswordEncoder};

let enc = BcryptPasswordEncoder::new(); // work factor 12 (the default)
let hash = enc.hash("s3cret").unwrap();
assert!(enc.verify("s3cret", &hash).unwrap());
assert!(!enc.verify("wrong", &hash).unwrap());

// Argon2id — the OWASP-preferred encoder, same PasswordEncoder port.
let argon = Argon2PasswordEncoder::new(); // OWASP defaults (m=19456, t=2, p=1)
let argon_hash = argon.hash("s3cret").unwrap();
assert!(argon_hash.starts_with("$argon2id$"));
assert!(argon.verify("s3cret", &argon_hash).unwrap());
```

## Recap — what changed in Lumen

This chapter closed Lumen's open front door without adding a dependency or a line
of business logic to the handlers:

| Before | After this chapter |
|--------|--------------------|
| any caller could open/deposit/withdraw/transfer | mutating routes require a `CUSTOMER` JWT; reads and `/actuator/*` stay public |
| no token machinery | one HS256 `JwtService` mints and verifies, auto-stamping a one-hour `exp` |
| no authorization | a path-ordered, fail-closed RBAC `FilterChain` plus method-level `#[pre_authorize]` / `#[post_authorize]` |
| — | the `FilterChain` + `BearerLayer` `#[bean]`s, auto-discovered and layered by `FireflyApplication` — no `with_security` call |

You also now know:

- That `JwtService::encode` auto-stamps a one-hour `exp` and `decode` rejects any
  token without one, so every credential is bounded.
- That `build_verifier` turns the service into a `Verifier` via `VerifierFn`,
  mapping `sub` → principal and `roles` → roles, with a bad token surfacing as
  `SecurityError::Verification` → a 401 problem.
- That `security_layers` composes a `BearerLayer` (with `allow_anonymous(true)`)
  and a first-match-wins `FilterChain`, where rule order decides who is permitted.
- That declaring the chain and layer as `#[bean]`s is the *entire* wiring — the
  framework sets the chain inside the correlation/headers edge and layers bearer
  auth on the outside (authenticate, then authorize).
- That method security pushes authorization onto domain operations through an
  ambient context the `BearerLayer` installs, so a service method enforces the
  rule without ever seeing the `Request`.
- That `verifier_from_config` / `bearer_layer_from_config` move the whole posture
  into `firefly.security.*`, so the demo key becomes a production IdP with no
  code edit.

## Exercises

1. **Add an ADMIN-only route.** Give Lumen a hypothetical `GET /api/v1/wallets`
   collection list and protect it with `require_pattern("/api/v1/wallets",
   &["ADMIN"])` so only an `ADMIN` may list every wallet, while `CUSTOMER` keeps
   access to the single-wallet read. Mint an `ADMIN` token in a test and assert a
   `CUSTOMER` token gets a 403.
2. **Role hierarchy.** Introduce a `SUPER` role that implies `CUSTOMER`. Build a
   `RoleHierarchy::from_string("SUPER > CUSTOMER")`, attach it with
   `chain.with_role_hierarchy(..)`, mint a `SUPER`-only token, and assert it
   passes the `require("/api/v1/wallets", &["CUSTOMER"])` rule.
3. **Swap in JWKS.** Sketch a `build_verifier_jwks()` that returns a
   `JwksVerifier::new("https://idp.example.com/.well-known/jwks.json")` and
   confirm (by reading the `Verifier` trait) that `security_layers` needs no other
   change. Why does the rest of Lumen not care which verifier it got?
4. **Expiry.** Lower the token lifetime with
   `JwtService::new(KEY).expiration_seconds(1)`, mint a token, wait two seconds,
   and assert the verifier now returns `SecurityError::Verification`.
5. **Method security in isolation.** Decorate a plain function with
   `#[firefly::pre_authorize(role = "CUSTOMER")]`, call it from a `#[test]`
   *without* a scope and assert `Unauthenticated`, then wrap the call in
   `firefly::security::with_authentication_scope_sync(auth, || ...)` with a
   `CUSTOMER` auth and assert it passes.

## Where to go next

A secure service is only trustworthy if you can *see* what it is doing. The next
chapter gives Lumen eyes and ears — structured logs, health, metrics, and the
admin dashboard.

- Make Lumen observable in **[Observability](./15-observability.md)** — the
  management surface beside the security perimeter you just built.
- Revisit how the framework discovers and wires beans like the `FilterChain` in
  **[Dependency Wiring](./04-dependency-wiring.md)**.
- Drive the wired router in tests with `bootstrap()` in **[Testing](./18-testing.md)**.
