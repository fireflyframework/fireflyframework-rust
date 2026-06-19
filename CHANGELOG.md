# Changelog

All notable changes to the Firefly Framework for Rust.

## v26.6.36 — 2026-06-20

**Spring Security parity — Tier 5b: SAML2 single sign-on (SP side).** The
Service-Provider half of the SAML 2.0 Web-Browser-SSO profile — Spring's
`saml2Login()` — delegating XML-signature verification to `samael` and adding a
Spring-faithful, hardened wrapper. Opt-in `saml2` feature; the default build is
unaffected. Adversarially reviewed before release.

### Added

- **`saml2` feature** (opt-in, pulls in `samael` + a system `libxml2` / `xmlsec1`
  / OpenSSL):
  - **`RelyingPartyRegistration`** + builder + **`InMemoryRelyingPartyRegistrationRepository`**
    (Spring's `RelyingPartyRegistration` / repository) — configured from IdP
    metadata XML or explicit asserting-party details.
  - **SP-initiated `AuthnRequest`** — `authn_request_redirect` (HTTP-Redirect
    binding) + **`Saml2AuthenticationRequestRepository`** (TTL'd outgoing
    request-id store for `InResponseTo` matching).
  - **`authenticate`** — verifies a POST-binding SAML `Response` (signature +
    audience / recipient / `InResponseTo` / status / time conditions, via
    `samael`) and maps the `NameID` + configured attributes to an
    `Authentication` (Spring's `OpenSaml4AuthenticationProvider`).
  - **`metadata_xml`** — SP metadata generation (Spring's `Saml2MetadataFilter`).
  - **`AssertionReplayCache`** + **`InMemoryAssertionReplayCache`** — one-time-use
    assertion replay protection.

### Security

- **Fail-closed on a missing IdP signing certificate**: building a registration
  is rejected when the asserting party has no signing cert, because `samael`
  would otherwise skip signature verification entirely (an authentication bypass).
- **Signature-algorithm allow-list** pinned to SHA-256+ RSA/ECDSA by default
  (`samael` otherwise accepts all algorithms — an algorithm-substitution risk).
- **One-time-use replay protection** the SAML profile requires but `samael` does
  not track; **size-bounded** response decoding; and all native XML-Security
  calls are **serialized** (the stack is not concurrency-safe).

### Notes

- Single-logout, signed `AuthnRequest`s, and encrypted assertions are follow-ups.
- Verification correctness rests on `samael` (whose own crypto suite covers
  accept/reject of XML signatures); this module's registration, mapping, replay,
  and rejection logic are unit-tested. The `saml2` feature's tests require the
  XML-Security system libraries and so run only when the feature is enabled.

## v26.6.35 — 2026-06-20

**Spring Security parity — Tier 5c: ACL / domain-object security.** The Rust
analog of `spring-security-acl`, answering `hasPermission(object, permission)`
from per-object access-control lists. Pure Rust — no new dependencies. All
additive. Adversarially reviewed before release.

### Added

- **ACL core** (`spring-security-acl` parity):
  - **`Permission`** — the `BasePermission` bitmask (`READ`=1, `WRITE`=2,
    `CREATE`=4, `DELETE`=8, `ADMINISTRATION`=16), with cumulative `union`,
    bit-`contains`, and case-insensitive name parsing.
  - **`Sid`** (`Principal` / `Authority` — Spring's `PrincipalSid` /
    `GrantedAuthoritySid`), **`ObjectIdentity`** (`type` + `identifier`),
    **`AccessControlEntry`** (sid + permission + granting), and **`Acl`** (owner
    + ordered ACEs + optional parent for inheritance).
  - **`AclService`** + **`InMemoryAclService`** (Spring's `MutableAclService`),
    and the free **`is_granted`** resolver.
- **`AclPermissionEvaluator`** — bridges an `AclService` to the Tier 3
  `PermissionEvaluator`, resolving `hasPermission(...)` against per-object ACLs
  by object reference *or* `(type, id)`. The principal and its roles/authorities
  map to `PrincipalSid` / `GrantedAuthoritySid` (each role matched both bare and
  `ROLE_`-prefixed).
- **`PermissionEvaluator::has_permission_for_id`** + the free
  **`has_permission_for_id`** — Spring's id-based `hasPermission` overload
  (default-deny, backward compatible).

### Security

- ACL evaluation is **default-deny**: a permission is granted only when an
  applicable *granting* entry is found (locally or up the inheritance chain);
  the **first entry matching a `(sid, permission)` wins**, so a deny placed
  before a grant takes precedence (Spring's `DefaultPermissionGrantingStrategy`).
  The inheritance walk is **bounded**, so a cyclic or pathologically deep parent
  chain terminates and denies rather than looping.

## v26.6.34 — 2026-06-19

**Spring Security parity — Tier 5a: LDAP / Active Directory authentication.**
The first of the Tier 5 "big subsystems", delivered as an opt-in feature. All
additive (no behaviour change to existing code; the default build does not
compile the new module). Adversarially reviewed before release.

### Added

- **`ldap` feature** (opt-in, pulls in `ldap3`) — Spring's
  `ldapAuthentication()`:
  - **`LdapAuthenticationProvider`** — bind authentication as an
    `AuthenticationProvider` (plugs into `ProviderManager`): search the user DN
    under a base+filter (`(uid={0})`, username RFC 4515-escaped), bind as that
    DN with the password (the directory verifies it), then map group membership
    (`(member={0})`) to `ROLE_<GROUP>` authorities — Spring's
    `BindAuthenticator` + `DefaultLdapAuthoritiesPopulator`.
  - **`ActiveDirectoryLdapAuthenticationProvider`** — binds as the
    `userPrincipalName` (`user@domain`) and maps the user's `memberOf` groups to
    roles.
  - **`LdapOperations`** port (+ `escape_filter_value`, `cn_from_dn`,
    `LdapEntry`) with the production **`Ldap3Operations`** adapter over `ldap3`.
    The port makes the provider logic unit-testable without a live directory.
- Security defaults: an **empty password is rejected before binding** (a simple
  bind with an empty password is an anonymous bind that most directories accept
  — an authentication bypass); the username/DN are RFC 4515-escaped in search
  filters (LDAP-injection safe); unknown-user and wrong-password fail with the
  same error value; a non-zero LDAP bind result code is an error (never a silent
  success).
- Hardened from the pre-release adversarial review: an **ambiguous user search**
  (more than one matching entry) is rejected rather than binding against an
  arbitrary first match (Spring's `IncorrectResultSizeDataAccessException`); a
  **directory error while populating authorities** propagates and fails the
  login instead of silently authenticating with no roles (Spring's
  `DefaultLdapAuthoritiesPopulator` semantics); and a **malformed directory
  entry** is caught and turned into a clean error rather than aborting the
  authentication task.

### Notes

- The live `Ldap3Operations` adapter is exercised by an integration test gated
  on `FIREFLY_TEST_LDAP_URL` (skipped when unset); the provider logic is fully
  covered by mock-`LdapOperations` unit tests.

## v26.6.33 — 2026-06-19

**Spring Security parity — Tier 4: the OAuth2 ecosystem.** The wider OAuth2
surface beyond the browser login flow. All additive (no behaviour change to
existing code). Adversarially reviewed before release.

### Added

- **Opaque-token introspection (RFC 7662)** — `RemoteTokenIntrospector`
  (Spring's `OpaqueTokenIntrospector`): POSTs a non-JWT bearer token to the
  authorization server's introspection endpoint (HTTP Basic client auth) and,
  on `active: true`, maps the response to an `Authentication`. Implements
  `Verifier`, so it drops into a `BearerLayer` as an alternative to local JWT
  verification. Fails closed (transport error / non-2xx / non-JSON /
  `active: false`/absent all reject).
- **Outbound OAuth2 client (`AuthorizedClientManager`)** —
  `OAuth2AuthorizedClientManager` + `OAuth2AuthorizedClientService` (+
  `InMemoryOAuth2AuthorizedClientService`) obtain, cache, and auto-refresh the
  access tokens the app needs to call downstream services: the client-credentials
  grant (service-to-service) and the refresh-token grant, reusing a cached
  `OAuth2AuthorizedClient` until it is within the clock-skew window of expiry.
- **RP-initiated logout (OIDC)** — `oidc_logout_url` + `ClientRegistration`'s new
  `end_session_endpoint` / `post_logout_redirect_uri`: `POST /logout` invalidates
  the local session and, when the provider advertises an `end_session_endpoint`,
  redirects to it with `id_token_hint` + `post_logout_redirect_uri` (Spring's
  `OidcClientInitiatedLogoutSuccessHandler`). The login callback now stores the
  `registration_id` + `id_token` for the hint.
- **Authorization-server HTTP endpoints** — `AuthorizationServerRouter` mounts
  the previously callable-only `AuthorizationServer` as `POST /oauth2/token`
  (RFC 6749; client-credentials + refresh-token, `client_secret_post`; RFC 6749
  §5.2 error envelope) and `GET /.well-known/oauth-authorization-server` (RFC 8414
  metadata).

### Security notes & known limitations (roadmap)

- The OAuth2 HTTP clients (introspection, outbound token, JWKS, login) now apply
  connect/read timeouts and cap the response body, so a slow or hostile endpoint
  cannot hang the bearer-verification path or force unbounded allocation; a token
  response with no `expires_in` is assumed short-lived (bounded fallback), never
  immortal. `ClientRegistration` and `OAuth2AuthorizedClient` redact their
  secrets/tokens in `Debug`.
- The authorization server signs HS256 (symmetric), so no `jwks_uri` is
  published; the server-side **authorization_code grant + PKCE**, an `/authorize`
  endpoint, and a client-authenticated `/oauth2/revoke` (RFC 7009) remain a
  follow-up.
- `OAuth2AuthorizedClientManager` does not single-flight concurrent
  authorizations for the same registration: concurrent callers may each hit the
  token endpoint, and against an authorization server that *rotates* refresh
  tokens, concurrent refreshes can lose a rotated token (last-writer-wins).
  Serialize refreshes for the same client if your AS rotates. Token-endpoint
  failures surface as the HTTP status (the structured RFC 6749 §5.2 error body is
  not yet parsed back).

## v26.6.32 — 2026-06-19

**Spring Security parity — Tier 3: method-security depth.** Expression-based
method security and domain-object permissions, the SpEL-equivalent layer over
the existing `#[pre_authorize]` / `#[post_authorize]` macros. All additive (no
behaviour change to existing code). Adversarially reviewed before release.

### Added

- **Expression-based `#[pre_authorize]`** — a non-keyword argument is now a
  boolean Rust expression evaluated *before* the body with the method's
  parameters and `auth` (a `&Authentication`) in scope (Spring's
  `@PreAuthorize("#id == authentication.name")`), e.g.
  `#[pre_authorize(auth.has_role("ADMIN") || auth.principal == owner)]`. The
  keyword rules (`authenticated`, `role`, `any_role`, `authority`,
  `any_authority`) are unchanged and fully backward-compatible. Fail-closed: no
  ambient context denies with `Unauthenticated`, a false expression with
  `Forbidden`.
- **`PermissionEvaluator` + `has_permission`** — the Rust analog of Spring's
  `PermissionEvaluator` / `hasPermission(target, permission)`. Register one
  process-wide with `set_permission_evaluator`; call
  `has_permission(auth, target, permission)` inside any pre/post expression. The
  target is erased to `Any` so one evaluator serves every domain type by
  downcasting. **Secure default: with no evaluator registered, every permission
  is denied.**
- **`#[pre_filter]` / `#[post_filter]`** — collection filtering (Spring's
  `@PreFilter` / `@PostFilter`). `#[post_filter(element.owner == auth.principal)]`
  retains only the elements of the returned collection the predicate accepts;
  `#[pre_filter(items, …)]` filters a named owned `mut` collection argument
  before the body. `element` is the per-element `&T` (Spring's `filterObject`);
  no ambient context denies the call with `Unauthenticated`.

### Known limitations (roadmap)

- `PermissionEvaluator` is a process-global set-once registry (one evaluator per
  process, like Spring's single bean); there is no per-scope override.
- `#[pre_filter]` requires the targeted parameter to be an owned `mut`
  collection with `retain` (e.g. `mut items: Vec<T>`).

## v26.6.31 — 2026-06-19

**Spring Security parity — Tier 2: the web authentication mechanisms.** The
classic browser/login surface from Spring's `HttpSecurity`, built on the Tier 1
authentication spine. All additive (no behaviour change to existing code).
Adversarially reviewed before release; the review's six confirmed findings are
fixed in this release.

### Added

- **HTTP Basic (`httpBasic()`)** — `HttpBasicLayer` reads
  `Authorization: Basic …` and authenticates through the `AuthenticationManager`
  spine. An **absent** header passes through (so a session/bearer layer can take
  over); an **invalid or malformed** one is rejected with `401` and a
  `WWW-Authenticate: Basic realm="…"` challenge (configurable realm, pluggable
  `BasicAuthenticationEntryPoint`) — Spring's `BasicAuthenticationFilter`.
- **Form login (`formLogin()`)** — `form_login_routes` mounts `POST /login`
  (url-encoded `username` + `password`), rotates the session id on success
  (anti-fixation) **before** persisting the context through a
  `SecurityContextRepository`, then redirects. Success/failure responses are
  swappable (`FormLoginSuccessHandler` / `FormLoginFailureHandler`), and the
  success path is saved-request-aware.
- **Remember-me (`rememberMe()`)** — `TokenBasedRememberMeServices` mints a
  signed, expiring cookie token whose signature is an **HMAC-SHA256** keyed by a
  server secret over the username, expiry, and the user's stored password hash:
  a password change, an expired clock, a tampered token, or the wrong key all
  reject. New trust-level methods on `Authentication` —
  `is_remembered()` / `is_fully_authenticated()` (+ `REMEMBERED_CLAIM`) — so a
  remembered context is authenticated but **not** fully authenticated (Spring's
  `isFullyAuthenticated()`), and a sensitive route can demand a fresh login.
- **`RequestCache` / `SavedRequest`** — `HttpSessionRequestCache` remembers the
  page an unauthenticated user wanted; form login returns them there after
  login instead of the default target (Spring's
  `SavedRequestAwareAuthenticationSuccessHandler`). Only **same-origin** targets
  are honoured (`SavedRequest::is_safe_redirect`): a protocol-relative,
  backslash-tricked, absolute, or control-char target falls back to the
  configured success URL, so the login flow can't be turned into an open
  redirect. `NullRequestCache` for stateless surfaces.
- **`SessionCreationPolicy`** — `Always` / `IfRequired` (default) / `Never` /
  `Stateless` (Spring's `sessionManagement().sessionCreationPolicy(...)`).
  `SessionAuthenticationLayer::session_creation_policy(...)` installs the implied
  `SecurityContextRepository`; `Stateless` uses the null repository (no session
  context) for token-only APIs.
- **Multiple filter chains** — `SecurityFilterChains` routes each request to the
  first chain whose `RequestMatcher` (`AnyRequestMatcher` /
  `PathRequestMatcher`, segment-aware, optional method) matches, so a
  locked-down `/api/**` and a permissive web surface coexist (Spring's
  `FilterChainProxy`); an unmatched request passes through. The dispatcher
  honours tower's readiness contract for a backpressure-bearing inner service.

### Known limitations (roadmap)

- `TokenBasedRememberMeServices` is the **stateless** of Spring's two
  remember-me strategies: a captured cookie replays for the full validity window
  (default 14 days) until the embedded expiry passes or the user's password hash
  changes — there is no per-token series/rotation theft detection (Spring's
  `PersistentTokenBasedRememberMeServices`) and no server-side revocation list.
  Use a short `token_validity_seconds` and serve the cookie `HttpOnly` + `Secure`
  + `SameSite`. A persistent/series variant is a follow-up.
- `RequestCache::save_request` is provided for an authentication entry point to
  call before redirecting to login; wiring an entry point that auto-saves the
  request is left to the application (the consume side is wired into form login).

## v26.6.30 — 2026-06-19

**Spring Security parity — Tier 1: the authentication spine.** The core of
Spring Security's authentication architecture, the foundation later tiers build
on. All additive (no behaviour change to existing code).

### Added

- **Authentication manager spine** — `AuthenticationManager` / `ProviderManager`
  / `AuthenticationProvider` (Spring's authentication architecture). An
  `AuthenticationRequest` (`UsernamePassword` / `BearerToken`, `#[non_exhaustive]`)
  is resolved by the first supporting provider; `BearerTokenAuthenticationProvider`
  adapts the existing `Verifier` into the spine.
- **`UserDetails` + DAO authentication** — `UserDetails` (with the four Spring
  account-status flags), `UserDetailsService`, `UserDetailsChecker` /
  `AccountStatusUserDetailsChecker`, `InMemoryUserDetailsService`, and
  `DaoAuthenticationProvider` (an enumeration-safe username/password provider:
  unknown user and wrong password both fail as `Bad credentials` with comparable
  bcrypt work).
- **`DelegatingPasswordEncoder`** — Spring's recommended `{id}`-prefixed password
  storage (`{bcrypt}`/`{argon2}`/`{noop}`), `upgrade_encoding` for re-hash-on-login,
  and seamless migration of legacy bare hashes; plus `NoOpPasswordEncoder`.
- **`SecurityContextRepository`** — the pluggable between-request context store
  (`HttpSessionSecurityContextRepository` default, `NullSecurityContextRepository`
  for stateless surfaces). `SessionAuthenticationLayer` now loads the context
  through a swappable repository instead of a hardcoded session key; added
  `Authentication::is_authenticated()`.
- **`AuthenticationEventPublisher`** — `AuthenticationEvent::{Success,Failure}`
  published by `ProviderManager` for every outcome (`LoggingAuthenticationEvent-
  Publisher` default).
- **Pluggable `AuthenticationEntryPoint` / `AccessDeniedHandler`** (Spring's
  `ExceptionTranslationFilter` seam) — `FilterChain` renders its `401`/`403`
  through them, defaulting to the canonical problem+json and overridable via
  `with_authentication_entry_point` / `with_access_denied_handler`.

### Known limitations (roadmap)

- `ProviderManager` continues to the next supporting provider after a failure
  (Spring rethrows an `AccountStatusException` immediately). With one provider
  per credential kind — the norm — the outcome is identical; a terminal/continue
  error taxonomy is a follow-up.
- `DelegatingPasswordEncoder::upgrade_encoding` compares the stored `{id}` only
  (algorithm migration); it does not yet flag a within-algorithm work-factor
  increase for re-hash.
- `with_defaults()` registers `{noop}` (plaintext — dev only, as Spring does)
  and verifies legacy *unprefixed* hashes as bcrypt to ease migration; disable
  the latter with `with_unprefixed(None)` for Spring's stricter reject-on-bare
  behaviour.

## v26.6.29 — 2026-06-18

A **Spring Security 6 parity** increment (Tier 0): an adversarially-verified
audit of the security tier against Spring Security 6 / Spring Boot 3, followed
by the hardening pass that closes the silent semantic divergences in shipping
code, plus the two Spring Security 6.4 passwordless mechanisms. See the new
**Spring Security Parity** book appendix for the full coverage matrix.

### Added

- **One-time-token (magic-link) login** — Spring 6.4 `oneTimeTokenLogin()`:
  `OneTimeTokenService` (single-use, expiring; in-memory impl) +
  `OneTimeTokenGenerationSuccessHandler` for out-of-band delivery +
  `ott_login_routes` (`POST /ott/generate`, `GET /login/ott`) that redeems a
  token, rotates the session id, and establishes the security context.
- **WebAuthn / passkeys** — Spring 6.4 `webAuthn()`: a feature-gated `webauthn`
  module with the registration and authentication ceremonies over `webauthn-rs`
  and a pluggable credential repository (opt-in; off by default).
- **EC + EdDSA JWKS keys** — `JwksVerifier` now verifies `ES256`/`ES384` and
  `EdDSA` tokens in addition to RSA (`RS*`/`PS*`).
- **`FilterChain::try_layer`** / **`CorsLayer::try_new`** — fallible builders
  that surface invalid glob patterns / unsafe CORS config as a recoverable
  error instead of panicking at startup.
- **Configurable clock-skew** (`clock_skew_seconds`, default 60s) and **`nbf`
  validation** on `JwksVerifier` and `JwtService`.
- A **Spring Security Parity** appendix in the book (EN + ES).

### Changed (Spring-faithful defaults — each with an escape hatch)

- **Method security works behind every authentication mechanism.**
  `SessionAuthenticationLayer` now scopes the task-local security context, so
  `#[pre_authorize]` / `current_authentication()` work for session- and
  OAuth2-login-authenticated callers (previously bearer-only).
- **`hasRole('X')` matches the `ROLE_X` authority** (Spring's prefix) as well as
  a bare role name.
- **HSTS is sent only over secure requests** by default
  (`hsts_include_insecure` to force it).
- **The CSRF cookie is `Secure` only when the request is secure**
  (`CookieSecure::{Auto,Always,Never}`, default `Auto`).
- **A wildcard CORS origin with `allow_credentials` is rejected** at
  construction.
- **JWT/JWKS validation tolerates 60s clock skew** (was zero).
- **Path-prefix authorization is segment-aware** — `permit("/api")` no longer
  matches `/api-internal`.

### Fixed (security)

- **OIDC `id_token` is never trusted without validation** — the login fails if
  it cannot be verified, instead of silently falling through to userinfo.
- **Bearer rejections carry an RFC 6750 `WWW-Authenticate: Bearer` challenge**
  (`error="invalid_token"` when a token was supplied).
- **No user-enumeration timing oracle** — an unknown username runs comparable
  bcrypt work to a wrong password (internal-db IdP).
- **Postgres `SessionRegistry` rows expire** (opt-in absolute TTL + pruning, via
  `with_ttl`) so an orphaned session can no longer inflate the per-principal
  concurrency count. Pruning is **off by default** — a fixed TTL would wrongly
  evict still-active *sliding* sessions, so enable it only with an absolute
  session lifetime.

### Known limitations (roadmap)

- `request_is_secure` trusts `X-Forwarded-Proto` from any caller; deploy behind a
  trusted proxy (or terminate TLS in-process, which Firefly marks automatically).
  A trusted-proxy allowlist is planned.
- WebAuthn `authenticate/options` reveals whether a username has registered
  passkeys; use discoverable (usernameless) credentials to avoid enumeration.
- Sliding-session expiry isn't synced into the distributed `SessionRegistry`
  (no `HttpSessionEventPublisher` analog yet) — deregister on logout or set an
  absolute TTL.
- One-time-token magic links are redeemed via `GET` (token in the URL);
  single-use + short expiry mitigate referer leakage.

## v26.6.28 — 2026-06-16

A Spring Boot **parity** increment: the declarative HTTP-interface client — the
highest single value lever from the parity-gap analysis (it lifts the REST/HTTP
clients area off the floor).

### Added

- **`#[http_client]`** — a declarative HTTP-interface client, the analog of
  Spring 6's `@HttpExchange` (the modern OpenFeign replacement). Annotate a
  **trait** of methods with the *same* verb attributes a `#[rest_controller]`
  uses and the macro generates a `<Trait>Impl` that issues the requests over a
  `WebClient` — the mirror image of a controller.
  - **Verbs:** `#[get("/path")]` / `#[post]` / `#[put]` / `#[delete]` /
    `#[patch]` + generic `#[request(method = "…")]`. Path variables use the
    framework's `:id` syntax (same as the server macro); `{id}` is a compile
    error pointing at `:id`.
  - **Argument binding** needs no attributes in the common case: a name-matched
    `:var` arg is the path variable, the lone non-scalar arg on a body verb is
    the JSON body, the rest are query params (`Option` omits when `None`,
    `Vec`/`&[_]` repeat). Override with `#[path]` / `#[query("k")]` /
    `#[header("X")]` / `#[body]`. Every `:var` must bind exactly once or it is a
    compile error; an `Option`/`Vec`/slice path variable is rejected.
  - **Return shapes:** `async fn -> Result<T, ClientError>` (the ergonomic
    default), `Result<T, E: From<ClientError>>`, non-async `Mono<T>` / `Flux<T>`
    (returned directly; a `Flux` defaults `Accept: application/x-ndjson`), and
    `WebClientResponse` (the `.exchange()` escape hatch).
  - **Construction:** `<Trait>Impl::new(base_url)` or `::with_client(WebClient)`;
    the type is `Clone`. With `#[http_client(... bean)]` it is registered as a
    `@Service` and bound to `dyn Trait`, so `#[autowired] Arc<dyn Trait>`
    resolves (pulling a shared `WebClient` bean, named via `client = "…"`).
  - **Error fidelity (documented):** an awaited `Result<T, ClientError>`
    surfaces every failure as `ClientError::Problem` (carrying a `FireflyError`
    with the original status/code, so the classifiers still work); the
    structured `Transport`/`Decode`/`Encode`/`InvalidUrl` variants survive only
    on the `Mono`/`Flux` return forms.
- **`firefly_client::encode_path_segment`** — RFC 3986 path-segment
  percent-encoding (used by generated clients; also public).

The macro reuses the server `#[rest_controller]`'s verb-attribute grammar
(`MappingAttr`/`VERBS`/`join_path`), so client and server can't drift. Designed
via a scored 3-proposal panel and adversarially reviewed (the review caught a
runtime footgun — an `Option` path variable producing `…/Some(x)` URLs — now a
compile error). The `firefly::prelude` now also re-exports `WebClient` /
`ClientError` / `new_web_client`.

## v26.6.27 — 2026-06-16

A Spring Boot **parity** increment: declarative rollback rules on
`#[transactional]`. Chosen from a 16-area parity-gap analysis as the best
value-to-effort gap (the transaction runtime already supported it).

### Added

- **`#[transactional(no_rollback_for = "<pat>", rollback_only_for = "<pat>")]`**
  — declarative transaction rollback rules. Spring names exception *types*;
  because Rust's `Result` already separates failure from success, the Firefly
  analog names an error **pattern**. By default every `Err` rolls back; then:
  - `no_rollback_for = "P"` — **Spring's `@Transactional(noRollbackFor = …)`**:
    an `Err` matching pattern `P` **commits** instead of rolling back;
  - `rollback_only_for = "P"`: roll back **only** when the `Err` matches `P`,
    committing the rest;
  - with both, `no_rollback_for` wins on overlap.

  The pattern is any Rust match pattern valid for the fn's error type (no `if`
  guard), alternatives included (`"Error::A | Error::B"`). The macro lowers to
  the already-present `transactional_with` / `transactional_with_on` runtime
  entry points (which take a `should_rollback(&E) -> bool` predicate), composes
  with `manager = "…"`, and the generated predicate is `matches!`-based, so a
  pattern that does not fit the error type is a compile error.

  `rollback_only_for` is **not** named `rollback_for`: Spring's `rollbackFor` is
  *additive* (it widens the set of exceptions that roll back), but Rust has no
  checked/unchecked split — every `Err` already rolls back — so the faithful
  rule here is *restrictive*. Writing `rollback_for` is a friendly compile error
  pointing at the two rules above, so a Spring port can't be silently inverted.
  No runtime or API changes elsewhere.

## v26.6.26 — 2026-06-16

A correctness release. Every one of the **74 per-crate `README.md`** files was
audited against that crate's *actual* shipped public API (43 confirmed fixes
across 28 crates), and the audit surfaced a real framework bug: the per-crate
`VERSION` constant was a hardcoded literal frozen at `"26.6.24"` instead of
tracking the crate version.

### Fixed

- **`VERSION` no longer drifts from the crate version.** Every crate's
  `pub const VERSION` was a hardcoded `"26.6.24"` string that nothing kept in
  sync with the workspace version — so `firefly_kernel::VERSION`, the actuator
  `/actuator/version` payload, and the startup banner all reported a stale
  release number, and the `version_matches_crate_version` guard tests only
  passed while the workspace happened to sit at `26.6.24`. All 52 hardcoded
  constants now derive from `env!("CARGO_PKG_VERSION")` (the re-exporting
  crates already chained to `firefly_kernel::VERSION`), so `VERSION` is now
  always exactly the crate version and can never drift again. The `cli`
  `FRAMEWORK_VERSION` constant got the same treatment, and the handful of
  unit/integration tests that asserted `VERSION == "26.6.24"` against a frozen
  literal now assert against `env!("CARGO_PKG_VERSION")`, so the guard holds for
  every release instead of only when the workspace happened to sit at that
  number. (The CLI's `render_for` / SBOM-parser fixtures keep their literal
  sample versions — that string is arbitrary test data, not the build version.)

- **Phantom / incomplete public-surface docs.** Documented APIs now match the
  source: `admin`'s `AdminDeps` gained its `environment` field; `openapi`'s
  `RouteDef` (4 missing fields: `request_schema` / `response_schema` /
  `query_schema` / `pageable`), `Parameter::{query,header}`, `Builder::{add_schema,
  add_schema_descriptors, from_inventory, docs_router}`, and the `DocsConfig`
  struct are now listed; `orchestration`'s `CompensationPolicy` (now all six
  variants incl. `GroupedParallel`) and `SagaError` (all four variants);
  `starter-web`'s `WebStack::{set_security, set_exception_advice}`; `testkit`'s
  `BuiltSlice::web_client`; `idp`'s `Error` enum + `change_password` signature;
  `security`, `webhooks`, `kernel`, `transactional`, `plugins` surface fixes.
- **Wrong signatures / variant names.** The `notifications-*` READMEs used the
  wire spellings `EmailStatus::SENT` / `FAILED`; the Rust variants are `Sent` /
  `Failed` (`#[serde(rename = "SENT")]` only affects the JSON). `plugins` showed
  a `Vec<Arc<dyn Any>>` annotation that does not type-check against the real
  `Extension = Arc<dyn Any + Send + Sync>`. `notifications-twilio`,
  `session-redis` parameter names corrected.
- **Wrong facts.** `admin`: the bean graph **does** ship dependency `edges`
  (one per autowired dependency), not "nodes-only". `backoffice`: the middleware
  order includes `TraceContext` (`Problem → TraceContext → Correlation →
  Idempotency → BackOffice`). `resilience`, `starter-core`, `eda-kafka`,
  `session-postgres`, `session-mongodb`, `container` (a `warm`→`form` typo) fixes.
- **Stale version pins.** Crate-README dependency examples that still pinned the
  long-stale `26.6.7` now use the self-maintaining minor pin `version = "26.6"`
  (the convention `firefly` / `testkit` / `webhooks` already used); example
  `VERSION` outputs updated to the release version.
- **`firefly-cache` doc comment.** Removed the stale "once the Redis adapter
  ships in the next minor" note — `firefly-cache-redis` (`RedisAdapter`) has
  shipped and is a published workspace member.

## v26.6.25 — 2026-06-16

A correctness-and-hygiene release: the workspace is `rustfmt`-clean again and
every book example was re-audited against the shipped API. No behavioural or
API changes.

### Fixed

- **Workspace formatting.** `cargo fmt --all --check` (and therefore
  `make ci`) was silently failing on `main`: prior changes to
  `firefly-openapi`, an observability test, and the `lumen-ledger` sample
  controller/`main.rs` were never formatted. Reformatted; `make ci` now passes
  `fmt-check` → `clippy -D warnings` → `build` → `test` end to end.
- **Book example accuracy (full-chapter audit, every finding verified against
  source).** Corrected: a `FireflyApplication::new(..).run()` missing `.await`;
  a `WalletView` read model missing its `Schema` derive; the CQRS middleware
  order (`ValidationMiddleware` is installed first by `Core`, so with the bus's
  reverse-iteration wrapping it is **outermost** — prose and the SVG diagram
  now read `Validation → Correlation → QueryCache`, not Correlation-first);
  three `WebClient` snippets that built a `Mono`/`Flux` without
  `.block().await`; the experience-tier signal endpoint path
  (`POST /journeys/:id/data`, not `/confirm`); a CLI `--url :8081` lacking a
  scheme; a streaming handler/test missing a `Response` import and the
  `open_with_deposit(&app)` argument; a stale `79 members` / “four samples”
  module-index count (now 86 / five reference samples); and two `samples/lumen`
  GitHub links that pointed at the org root instead of the file.

## v26.6.24 — 2026-06-16

Spec-based **filtering** is now first-class on derived repositories, with a
filtering endpoint in the sample.

### Added

- **`#[derive(SqlxRepository)]` also implements `ReactiveSpecificationRepository`**
  (`find_by_spec` / `find_by_spec_paged`) by delegation — so any derived
  repository runs composable, dialect-aware `Specification` queries out of the
  box (the Spring Data `JpaSpecificationExecutor` analog), not just CRUD +
  derived queries.
- **lumen-ledger `GET /api/v1/wallets/search`** — a `WalletFilter` query DTO
  (owner / currency / status / minBalance / maxBalance, each an OpenAPI query
  parameter) that the `@Service` turns into a `firefly::data::Specification` the
  repository compiles to a `WHERE`. At least one criterion is required — a
  no-filter search is a **422**, not an unscoped list-everything (and, like the
  rest of this auth-free demo, a real service would authorization-scope the
  results). New `WalletFilter` DTO; `WalletService::search`.

## v26.6.23 — 2026-06-16

OpenAPI **operation parameters & bodies** now render in full (Swagger UI / ReDoc
showed bare operations with no inputs before).

### Added

- **Query parameters** are generated from a `Query<T>` / `ValidQuery<T>`
  extractor: the generator expands `T`'s `#[derive(Schema)]` fields into one
  `in: query` parameter each (required iff non-optional). A `PageRequest`
  argument adds the standard `page` / `size` / `sort` parameters.
- **Header parameters** are declared on a verb attribute:
  `#[post("/x", header("Idempotency-Key", required, description = "…"))]` (and
  `query("…")` for an extra query parameter) → an `in: header` parameter the
  handler reads like any axum header.
- `RouteDescriptor` gains `query_schema` / `pageable` / `parameters`
  (`ParamDescriptor`); the openapi `Builder` expands them; `Parameter::{query,
  header}` constructors.

### Fixed

- **Request bodies are inferred from `Valid<T>`**, not only `Json<T>` — so the
  validating extractor's body (the common case) is documented as a
  `requestBody`. POST/PATCH operations previously showed no body in Swagger UI.

### lumen-ledger

- Query DTOs (`OwnerQuery`, `StatusQuery`) and `StatusBody` are `#[derive(Schema)]`
  so their fields render; `POST /wallets` declares an `Idempotency-Key` header.

## v26.6.22 — 2026-06-16

Security: remove an unauthenticated all-wallets listing from the sample
(flagged by automated security review of 26.6.21).

### Fixed

- **lumen-ledger**: `GET /api/v1/wallets` is **owner-scoped again** —
  `?owner=` is required. 26.6.21 had made it list *every* wallet when `owner`
  was omitted (`WalletService::list_all`), an unauthenticated enumeration of all
  account holders + balances (IDOR / broken access control). The unfiltered
  listing and `list_all` are removed; a missing `owner` now returns a **clear**
  RFC 9457 `400` (`the \`owner\` query parameter is required …`) instead of the
  confusing raw `missing field owner` deserialization error — the actual DX issue
  behind the original report. The controller documents why an unfiltered listing
  must be authorization-scoped (admin authority + caller-scoping) in a real
  service, which this auth-free feature sample intentionally does not wire.

## v26.6.21 — 2026-06-16

API docs move to the **management** port, and the surfaces are hardened.

### Changed

- **OpenAPI docs (Swagger UI / ReDoc / `/v3/api-docs`) are served on the
  management port**, beside actuator + admin — not the public API port. They
  expose the whole API surface and every schema, a control-plane concern, so the
  public data-plane port no longer serves them.
- The OpenAPI document now declares the **API base URL** as its `server`
  (`Builder::add_server`), so Swagger UI's *Try it out* / ReDoc target the API
  port rather than the management origin the docs load from. Derived from the API
  bind address (wildcard host → `localhost`); overridable with
  `FIREFLY_OPENAPI_SERVER_URL`.

### Fixed

- The **management** listener now answers an RFC 9457 `application/problem+json`
  **404** for unknown paths (it previously returned axum's bare empty body),
  matching the public API.
- **lumen-ledger** `GET /api/v1/wallets` lists every wallet when `?owner=` is
  omitted (an optional filter) — a bare collection request is a 200, not a
  "missing query parameter" 400. Adds `WalletService::list_all`.

## v26.6.20 — 2026-06-16

The lumen-ledger sample gains the **transactional transfer** use case, on a new
`#[transactional]` option that binds the boundary to an explicit manager.

### Added

- **`#[transactional(manager = "<expr>")]`** (firefly-macros) — Spring's
  `@Transactional("txManager")`. Drives an **explicit** `TransactionManager` (the
  expression `m` yields a value with `&m: &Arc<dyn TransactionManager>`, e.g.
  `self.tx_manager()`) via `transactional_on`, instead of the process-global
  registry. For a multi-datasource service, or to keep per-instance / per-test
  isolation.
- **lumen-ledger `transfer`** — `POST /api/v1/wallets/:id/transfer` +
  `WalletService::transfer` move funds between wallets **atomically** under
  `#[transactional(manager = "self.tx_manager()")]`: the debit and credit commit
  together or not at all, and a rejected transfer (insufficient funds, inactive
  party, self-transfer, bad destination) moves no money and renders RFC 9457
  **422**. New `TransferRequest` DTO; the service autowires the `Db` to build its
  own manager.

### Docs

- Book: the layered-microservices web-surface table gains an *atomic transfer*
  row; the declarative-macros `#[transactional]` section documents the `manager`
  option; the persistence-config note explains the per-instance-manager choice.

## v26.6.19 — 2026-06-16

Spring Boot **parity** push, PR 9/N — **Tier B**: Actuator DI/route introspection.

### Added

- **`/actuator/beans`, `/actuator/mappings`, `/actuator/conditions`**
  (firefly-actuator) — Spring Boot Actuator's introspection endpoints, rendered
  from the framework's compile-time inventory (`firefly_container::{discovered,
  routes}`), so they need no live container:
  - **`beans`** — every DI bean (type, module, scope, stereotype, primary, lazy),
    grouped under `contexts.application.beans`.
  - **`mappings`** — every `#[rest_controller]` route (method, path, controller,
    handler, summary), the `RequestMappingHandlerMapping` analog.
  - **`conditions`** — the `@Profile` / `@ConditionalOn…` guards each
    conditionally-registered bean declares.
  - `mount()` auto-registers all three (override-respecting); each is served only
    when the `ExposureConfig` includes it, exactly as Spring gates them behind
    `exposure.include`. Also exposed via `register_introspection` /
    `BeansEndpoint` / `MappingsEndpoint` / `ConditionsEndpoint`.

This completes the prioritized **Tier A + Tier B** Spring-Boot-parity gap list.

## v26.6.18 — 2026-06-16

Spring Boot **parity** push, PR 8/N — **Tier B**: `@Validated` config properties.

### Added

- **`#[derive(ConfigProperties)]` + `#[firefly(validate)]`** (firefly-macros) —
  Spring's `@ConfigurationProperties @Validated`. After binding the struct from
  config, its declarative `#[derive(Validate)]` constraints run; a violation
  **fails the bean's creation** (context refresh) with the structured per-field
  errors, instead of letting an out-of-range setting reach the app. Requires the
  struct to also `#[derive(Validate)]`.

## v26.6.17 — 2026-06-16

Spring Boot **parity** push, PR 7/N — **Tier B**: caching `condition` / `unless`.

### Added

- **`#[cacheable(condition = "...", unless = "...")]`** (firefly-macros) — Spring's
  `@Cacheable` conditional caching:
  - **`condition`** — a Rust boolean over the method parameters, evaluated
    *before* any cache interaction; `false` bypasses the cache entirely (no read,
    no write — just the body).
  - **`unless`** — a Rust boolean over the freshly computed value (bound as
    `result: &V`), evaluated *after* the body; `true` returns the value but does
    **not** store it.
  - Both are `#[cacheable]`-only (rejected on `#[cache_put]` / `#[cache_evict]`
    with a clear error).

## v26.6.16 — 2026-06-16

Spring Boot **parity** push, PR 6/N — **Tier B**: an Argon2id password encoder.

### Added

- **`Argon2PasswordEncoder`** (firefly-security) — Spring Security's
  `Argon2PasswordEncoder`, the OWASP-preferred memory-hard alternative to
  `BcryptPasswordEncoder`, behind the same `PasswordEncoder` port. Produces
  self-describing Argon2id PHC strings (`$argon2id$v=19$m=…,t=…,p=…$…`) so a hash
  still verifies after the encoder is reconfigured; `new()` uses the `argon2`
  crate's OWASP defaults, `with_params(m, t, p)` sets explicit cost.

## v26.6.15 — 2026-06-16

Spring Boot **parity** push, PR 5/N — **`@Transactional` ↔ repository
integration, proven**. The `#[transactional]` macro and the `firefly-data-sqlx`
repository were each tested in isolation but never *together*; this adds the
end-to-end coverage and corrects a stale design note it disproves.

### Added

- **`firefly-data-sqlx/tests/transactional.rs`** — proves the transactional
  runtime drives the sqlx repository over a real SQLite database:
  - a write inside a **rolled-back** transaction is undone (and is visible
    *within* its own transaction before the rollback);
  - a **committed** transaction persists;
  - a **non-transactional** write stays immediately visible to a later read even
    with a process-global manager registered (disproving the lumen-ledger note's
    "invisible write" claim).
  - The per-database tests drive an **explicit** manager via `transactional_on`
    rather than the first-wins process registry, the isolation-safe pattern for a
    multi-datasource / per-test suite.

### Changed

- **lumen-ledger**: the persistence config's design note is corrected — it cited
  a (disproven) ambient-enlistment visibility bug; the real reason the sample
  keeps `@Version` optimistic locking instead of registering a manager is that
  the manager registry is process-global first-wins, which does not fit a test
  suite where every test boots its own isolated in-memory database. The note now
  points to the new integration tests and shows the production pattern (register
  once at startup, annotate with `#[firefly::transactional]`).

## v26.6.14 — 2026-06-16

Spring Boot **parity** push, PR 4/N — **test slices**. Completes the
`@WebMvcTest` / `@MockBean` story: the `Slice` already provided DI slices and the
mock-bean override (`instance` + `bind`); this adds the bridge from a controller
slice to an in-process `MockMvc`.

### Added

- **`BuiltSlice::web_client::<C, _>(C::routes)`** (firefly-testkit, feature
  `web`) — Spring's `@WebMvcTest(C)`: resolves the controller bean `C` from the
  slice (so its collaborators are the installed mocks) and wraps its
  `#[rest_controller]`-generated router in a `TestClient`, exercising one
  controller's whole web layer over fakes with no full-application boot and no
  datasource.

### Docs

- The testing chapter documents the `@WebMvcTest` (`web_client`), `@MockBean`
  (`instance` + `bind`), and `@DataJpaTest` (a `Slice` over an in-memory SQLite
  repository) mappings.

## v26.6.13 — 2026-06-16

Spring Boot **parity** push, PR 3/N — **web developer experience**. Three
argument resolvers / extractors that close the gap with Spring MVC's binding
layer, all rendering failures as the framework's RFC 9457 `application/problem+json`.

### Added

- **`PageRequest`** (firefly-web) — Spring Data Web's `Pageable` argument
  resolver. Binds `?page=&size=&sort=` (1-based `page`, `size` capped at 2000,
  **repeatable** `sort=property[,asc|desc]`) into a `firefly_data::Pageable`; a
  bad value is a **400** problem. `firefly-web` now depends on `firefly-data`.
- **`ValidPath<T>` / `ValidQuery<T>`** (firefly-web) — `@Valid` on a path/query
  object: extract like `Path<T>` / `Query<T>` (malformed bind → **400**), then
  run the type's declarative `Validate` constraints (failure → **422** with the
  structured violations), the twin of the `Valid<T>` JSON extractor.
- **`Multipart` / `UploadedFile`** (firefly-web) — a `@RequestParam MultipartFile`
  analog that **drains** a `multipart/form-data` request up front into named text
  fields (`text(name)`) and uploaded files (`file(name)` / `files()`), turning any
  decode failure into a **400** problem instead of axum's escaping streaming error.
- All four are re-exported from the `firefly::prelude`.

### Changed

- **lumen-ledger**: the `WalletService::list_by_status` signature now takes a
  `Pageable` (was `page: usize, size: usize`), and the controller's paged
  endpoint binds it with `PageRequest` — so `?sort=balance,desc` flows
  end-to-end to the repository (covered by the in-process integration test).

## v26.6.12 — 2026-06-16

Spring Boot **parity** push, PR 2/N. The `firefly_resilience` primitives gain a
declarative face: Resilience4j / Spring-Retry **decorator macros**, so a guard is
one annotation on a method instead of a hand-built `execute(op)` at every call.

### Added

- **`#[retry]` / `#[circuit_breaker]` / `#[rate_limit]` / `#[bulkhead]` /
  `#[timeout]`** (firefly-macros) — the `@Retry` / `@CircuitBreaker` /
  `@RateLimiter` / `@Bulkhead` / `@TimeLimiter` annotations. Decorate an
  `async fn` returning `Result<T, E>` where
  `E: std::error::Error + Send + Sync + 'static + From<ResilienceError>`:
  - The body's own failure threads through the guard as
    `ResilienceError::Operation` and the **original `E` is recovered** on the way
    out (the caller still pattern-matches the domain error); a guard's own
    short-circuit (timeout / open circuit / rate-limit / bulkhead-full) surfaces
    through `E::from(ResilienceError)`.
  - The attributes **stack** (outermost first), e.g. `#[retry]` over
    `#[circuit_breaker]`.
  - The **stateful** guards (`#[circuit_breaker]`, `#[rate_limit]`,
    `#[bulkhead]`) keep their state in a per-method `static`, shared across every
    call — the Resilience4j registry-bean semantics; `#[retry]` and `#[timeout]`
    are stateless and rebuilt per call.
  - Durations accept a unit-suffixed string (`"100ms"`, `"2s"`, `"1m"`, `"1h"`)
    or a bare integer of milliseconds.

## v26.6.11 — 2026-06-16

The first of a multi-PR **Spring Boot parity** push (driven by a framework-wide
audit). This one completes the Spring Data repository story: the entity is now
*just annotated fields*.

### Added

- **`#[derive(Entity)]`** (firefly-macros) — generates the `SqlxEntity` mapping
  (`@Table` / `@Id` / `@Version` / `@Column`) from a struct's fields, the JPA
  `@Entity` experience. Scalar columns (`String`, `i64`/`i32`, `bool`, `f64`,
  `Uuid` as text, `DateTime<Utc>` as text) map automatically; `#[firefly(id)]`,
  `#[firefly(version)]`, and `#[firefly(column = "...")]` annotate the key,
  optimistic-lock column, and renames; a non-scalar field (e.g. an enum) uses
  `#[firefly(with(read = "...", write = "..."))]`. Pairs with
  `#[derive(SqlxRepository)]` so a repository is declared, not hand-built.
- **`firefly_data_sqlx::parse_timestamp`** — the text-portable timestamp decode
  the derive uses for `DateTime<Utc>` columns (tolerates RFC 3339 and the
  space-separated auditor form across SQLite/PostgreSQL).

### Changed

- **lumen-ledger**: the `Wallet` entity's ~50-line hand-written `SqlxEntity` impl
  is now a `#[derive(Entity)]` over annotated fields.

## v26.6.10 — 2026-06-16

The **Spring Data repository** pass. A hand-built repository declared with a
`#[bean]` factory is *not* how Spring Data reads — you declare a repository over
an entity and the framework supplies the implementation. Firefly does that now.

### Added

- **`#[derive(SqlxRepository)]`** (firefly-macros) — turns a struct holding a
  `SqlxReactiveRepository<Entity, Id>` into a fully-wired **`@Repository` bean**:
  discovered by the scan and classified as `@Repository` in `/beans`, **built
  from the injected `Db` datasource bean** (table config + `@Version` optimistic
  locking + `@CreatedDate`/`@LastModifiedDate` auditing, all wired from the
  entity), and implementing `ReactiveCrudRepository` by delegation. The Spring
  Data "declare a repository, get the implementation" experience — no `#[bean]`
  factory, no hand-written CRUD.
- **`SqlxEntity` + `repository_for`** (firefly-data-sqlx) — the
  `@Table`/`@Id`/`@Version`/`@Column` entity contract and the one-call factory
  the derive builds from.

### Changed

- **lumen-ledger**: the repository is now `#[derive(SqlxRepository)]` over a
  `Wallet` that `impl SqlxEntity`; the `#[bean]` moved to the **`Db` datasource**
  (Spring Boot's auto-configured `DataSource`), which is the only async bean.
- `ApplicationContextBuilder::build()` no longer *panics* on a pending async bean
  (a shared test binary's inventory could trip it for unrelated tests). It
  documents that the synchronous path does not await async beans — use
  `build_async()`; an un-awaited async bean now fails discoverably at resolve
  time (`NoSuchBean`) rather than at build.

> Tracked next: a `#[derive(Entity)]` to generate the `SqlxEntity` mapping from
> the entity's fields. Today the entity declares its column mapping explicitly,
> like JPA `@Column`s.

## v26.6.9 — 2026-06-15

The **Spring Boot fidelity pass**. A multi-lens audit (with every finding
adversarially verified against the source) of the layered `lumen-ledger` sample
and the v26.6.8 framework surfaced — and this release closes — the gaps between
"compiles" and "behaves like a Spring Boot service".

### Added (framework)

- **Problem-rendering `Path<T>` / `Query<T>` extractors** (`firefly::web`).
  Drop-in replacements for axum's: a malformed path segment (a non-UUID where a
  `Uuid` is expected) or a missing/un-parseable query parameter now renders a
  **400 RFC 9457 problem** instead of axum's plain-text rejection — the Rust
  analog of `MethodArgumentTypeMismatchException` going through the same advice.
- **`firefly_data_sqlx::is_optimistic_lock(&err)`** — detects the optimistic-lock
  conflict the **reactive** `save` surfaces through its `FireflyError` channel
  (the blocking `save` already returned `DataError::OptimisticLock`), so a service
  can map a stale `@Version` write to a domain `409` instead of a generic `500`.
- **`#[bean(stereotype = "…")]`** — overrides a bean's admin `/beans`
  classification, so an async-constructed data-access bean still reads as
  `@Repository`.
- **`ApplicationContext::build_async()` / `testkit::Slice::build_async()`** —
  await every `async fn #[bean]` (via `Container::init_async_beans`) off the
  `FireflyApplication` bootstrap path. The synchronous `build()` now **fails fast**
  (panics) if async beans are pending rather than silently dropping them — Spring's
  single refresh lifecycle, where every singleton is initialised before the
  context is handed back.
- **`ContainerError::BeanCreation`** — an async-bean factory failure is wrapped
  with the bean's identity ("error creating bean '…': <cause>"), Spring's
  `BeanCreationException`.

### Changed (sample — `lumen-ledger`)

- **`@Version` optimistic locking** (`with_version_column`) and **store-side
  auditing** (`with_auditor`) are wired onto the repository: the service no longer
  hand-bumps `version` or stamps `created_at`/`updated_at`, and a concurrent stale
  write is rejected as **409** (proven by a new repository test).
- **`Wallet.status` is a typed `WalletStatus` enum**, converted token↔enum
  exactly once at the `RowMapper`/`RowWriter` boundary (`@Enumerated(STRING)`).
- **Bean validation at the edge**: `Valid<AmountRequest>` (`range(min = 1)`),
  currency `pattern("[A-Z]{3}")`, opening-balance `range(min = 0)` — each a 422
  before the service runs; the new `Path`/`Query` extractors render 400s.
- **A fuller REST surface**: a status-transition `PATCH /…/status`, a `DELETE`
  (204), and a paginated `GET /…?status=&page=&size=` returning a Spring-Data
  `Page<T>` (built from the `find_by_status` paged derived query). `ServiceError`
  gains a `Conflict` (409) variant.

### Docs

- The Layered Microservices chapter now covers the production-grade web surface,
  `@Version`/auditing, and the full endpoint set; persistence documents generic
  `SqlKey` keys + optimistic locking; the DI chapter documents `build_async` +
  the stereotype override; the OpenAPI chapter documents enum schemas + the
  `firefly openapi-client` generator.

> Tracked for a follow-up: fallible `Result<T, E>` bean factories, OpenAPI
> per-operation response codes + `#[schema(example = …)]` enrichment, and a
> versioned (Flyway-style) migration runner.

## v26.6.8 — 2026-06-15

The **layered-microservices milestone**. Firefly can now be built the way a
firefly-oss core service is — split into `-interfaces` / `-models` / `-core` /
`-web` / `-sdk` crates, one public type per file (Java-style) — with the
framework gaining the pieces that real layered services need: async beans,
unbounded repository keys, and an OpenAPI→client generator.

### Added

- **Async beans (`async fn #[bean]`).** A `#[bean]` factory may now be `async`:
  the container parks it during the synchronous `scan()` and `await`s it during
  the new `Container::init_async_beans()` (run by `FireflyApplication` right
  after the scan), then publishes the result as a ready singleton. Async beans
  are sequenced by `#[bean(order = N)]`, so one may autowire another initialised
  earlier. This is Spring Boot's "a `@Bean` does I/O at context-refresh time",
  with the I/O `await`ed rather than blocking a thread — the idiomatic way to
  wire a connection pool, broker dial, or warmed cache.
- **Unbounded repository keys (`SqlKey`).** `SqlxReactiveRepository<T, ID>` /
  `SqlxRepository<T, K>` accept any `serde::Serialize` key through the new
  blanket-implemented `SqlKey` trait, so `Uuid`, `i64`, `String`, an enum, or a
  composite-key struct all work as the `ID` — matching the unbounded `ID` of a
  Spring Data `CrudRepository<T, ID>` (the MongoDB adapter already accepted any
  `Serialize` key; the two adapters are now consistent).
- **`firefly openapi-client`.** A new CLI subcommand generates a self-contained
  typed Rust client from an OpenAPI 3.x document — a model `struct`/`enum` per
  `components.schemas` entry and one `async fn` per operation over
  `firefly_client::RestClient`, with typed path/query parameters and JSON
  bodies. The Rust analog of firefly-oss's OpenAPI-generated WebClient SDK.
- **`#[derive(Schema)]` for enums.** Field-less enums now emit a JSON Schema
  `string` enumeration into the OpenAPI document (honouring serde `rename_all` /
  per-variant `rename`), so a DTO enum field is no longer an unresolved `$ref`.
- **`lumen-ledger` sample.** A complete layered wallet/ledger microservice
  (`samples/lumen-ledger/`): five crates, one public type per file under
  `<domain>/v1` paths, a real sqlx repository published as an async bean
  (in-memory SQLite by default, `DATABASE_URL=postgres://…` for PostgreSQL),
  `@Service`/`@Mapper`/`@Component`/`@RestController`/`@Configuration` stereotypes,
  a typed SDK, and a cross-crate `firefly::link!` integration test.
- **Book.** New "Layered Microservices" chapter; the dependency-wiring chapter
  now explains the `firefly::link!` dead-strip rule, and the DI chapter documents
  async beans.

### Fixed

- **`#[derive(Validate)]` string constraints.** The derive emitted
  `::core::format!`, which does not exist (`format!` needs `alloc`/`std`), so any
  `#[validate(not_empty | length | email | …)]` failed to compile. Now emits
  `::std::format!`.

## v26.6.7 — 2026-06-15

The **everything-under-DI milestone**. CQRS handlers and EDA listeners can now be
methods on a `@Component`-style bean that autowires its collaborators — the last
piece that lets a service wire *every* component through the DI container, exactly
like Spring Boot, with no process-globals.

### Added

- **`#[handlers]` — bean-based CQRS / EDA handlers.** Apply it to the `impl`
  block of a registered bean (e.g. a `#[derive(Service)]` whose collaborators are
  `#[autowired]`); each `#[command_handler]` / `#[query_handler]` (a CQRS message
  handler) or `#[event_listener("topic")]` (an EDA listener) method takes `&self`
  plus one message / event. `FireflyApplication` resolves the bean from the
  container and installs each handler, so a handler reaches its collaborators
  through ordinary `#[autowired]` fields — the Rust analog of Spring scanning a
  `@Component`'s `@CommandHandler` / `@EventListener` methods.
- **Bean handler/listener discovery** — `firefly_cqrs::{BeanHandlerRegistration,
  register_discovered_handler_beans}` and `firefly_eda::{BeanListenerRegistration,
  subscribe_discovered_listener_beans}`, drained by `FireflyApplication` after the
  container is scanned (alongside the existing free-`fn` discovery). The startup
  report counts bean handlers / listeners too.

### Changed

- **Lumen is now fully DI-wired.** The CQRS handlers are a `WalletHandlers`
  `#[derive(Service)]` bean and the read-model projection is a `WalletProjection`
  `#[derive(Service)]` bean, each `#[autowired]`-ing the `Ledger` + `ReadModel`.
  The `OnceLock` process-globals (`commands::bind` / `effective_read_model` /
  `bind_projection`) and the free-`fn` handlers / projection are gone, and the
  `ledger` `#[bean]` is a pure factory. The HTTP tests boot **one** app context
  per test (Spring Boot's `@SpringBootTest` model) and drive every request against
  it, so one container's singletons stay consistent.
- The free-`fn` `#[command_handler]` / `#[query_handler]` / `#[event_listener]`
  macros are unchanged and still supported for simple, collaborator-free handlers.
- Lumen's read model is now a `#[derive(Repository)]` (`@Repository`) data-access
  bean rather than a `@Bean` factory product, so the sample exercises the full
  Spring stereotype set — `@Configuration` + `@Bean`, `@Service`, `@Repository`,
  and `@Controller` + `@Autowired` — all scanned and wired by the DI container.

## v26.6.6 — 2026-06-15

The **turnkey-bootstrap & auto-generated-API-docs milestone**. A service now
boots from a single line — `firefly::FireflyApplication::new("app").run().await`
— and the framework discovers, wires, and serves everything Spring Boot's
`SpringApplication.run` would: component scan, controller auto-mount, handler /
listener / scheduled draining, security + middleware, the self-hosted admin
dashboard, and now a fully **auto-generated OpenAPI surface** and a transparent
**global exception-advice** layer. No composition root, no `build_app`, no
manual route registration.

### Added

- **`FireflyApplication` — the turnkey bootstrap** (Spring's
  `SpringApplication.run`). `new(name).version(v).run().await` builds the web
  stack, auto-registers the infrastructure beans, component-scans the app's
  beans, drains the inventory-registered CQRS handlers / EDA listeners /
  `#[scheduled]` tasks, auto-mounts every `#[rest_controller]`, auto-discovers
  the security `FilterChain` + `BearerLayer` beans, installs the correlation /
  W3C-trace / read-cache middleware, self-hosts the admin dashboard on the
  management port, prints a pyfly/Spring-style line-by-line startup report, and
  serves the public + management ports with graceful shutdown.
  `bootstrap()` returns the assembled (un-served) app for in-process tests.
- **Auto-generated OpenAPI 3.1 + Swagger UI + ReDoc**, wired automatically into
  every app (the springdoc-openapi model — no application code). The spec is
  built from the live inventory (`#[rest_controller]` routes +
  `#[derive(Schema)]` DTOs) and served at `/v3/api-docs` (+ `/openapi.json`
  alias), with Swagger UI at `/swagger-ui` (+ `/swagger-ui.html`) and ReDoc at
  `/redoc`.
- **`#[derive(Schema)]`** — registers a DTO's OpenAPI component schema
  (springdoc's `@Schema`), computed at compile time (no runtime reflection) by
  walking the struct's fields, honouring serde `rename` / `rename_all` / `skip`,
  and `$ref`-ing nested `#[derive(Schema)]` types. Every registered schema lands
  in the document's `components.schemas`.
- **Request / response model inference** — the `#[rest_controller]` macro infers
  each operation's request and response schema from the handler signature (the
  `Json<T>` parameter and the `Json<T>` in the `WebResult<…>` / tuple return
  type); a `$ref` is emitted only when the type is a registered `Schema`, so an
  unannotated body (e.g. `serde_json::Value`) never dangles.
- **Per-operation OpenAPI metadata on the verb macros** —
  `#[get("/x", summary = "…", description = "…", tags = ["…"], status = 200,
  deprecated, request = T, response = T)]` and a `#[rest_controller(tag = "…")]`
  group tag. `request` / `response` are optional overrides of the inference.
- **Global exception-advice layer** (Spring's `@ControllerAdvice`) — register an
  `ExceptionHandlerRegistry` bean and `FireflyApplication` installs an
  `ExceptionAdviceLayer` at the outermost edge that re-parses every
  `application/problem+json` response and re-renders it through the registry
  (custom status / title / body), preserving existing response headers.
- **Default RFC 9457 `404`** — an unmatched route now returns a proper
  `application/problem+json` not-found document (rendered identically to every
  other framework error) instead of axum's bare empty body.

### Changed

- The Lumen sample is now a single-binary crate with a **one-line `main`**; its
  HTTP surface (`web.rs`) is purely declarative — `#[derive(Configuration)]` +
  `#[bean]` factories, a `#[derive(Controller)]` + `#[autowired]` controller,
  `FilterChain` / `BearerLayer` beans, a feature-gated `RouteContributor` bean,
  and `#[derive(Schema)]` DTOs annotated with per-operation OpenAPI metadata.
- Bind addresses are overridden with `FIREFLY_SERVER_ADDR` /
  `FIREFLY_MANAGEMENT_ADDR` (honoured by `FireflyApplication`).

## v26.6.5 — 2026-06-15

The **declarative-services milestone**. A complete declarative layer lands on top
of the standalone framework: annotation-style orchestration, in-process events
with a transactional/broker bridge, aspect-oriented advice, caching, validation,
and async methods — each a thin macro over a real, tested engine. The book and
all reference docs are brought current.

### Added

- **Declarative orchestration** — `#[saga]` + `#[saga_step]` (DAG `depends_on`,
  compensation, retry/backoff/timeout, argument injection via
  `#[input]`/`#[from_step]`/`#[variable]`/`#[ctx]`), `#[workflow]` +
  `#[workflow_step]` (parallel DAG), and `#[tcc]` + `#[participant]`
  (try/confirm/cancel). The `Saga` engine gained layered topological execution
  (`Step::depends_on`); the Lumen sample now drives its transfer (saga),
  compliance (workflow), and two-phase transfer (TCC) declaratively.
- **In-process application events** — `#[application_event_listener]`
  (Spring `@EventListener`) and `#[transactional_event_listener]`
  (`@TransactionalEventListener`, phases `before_commit` / `after_commit` /
  `after_rollback` / `after_completion`), `publish_event`, an `inventory`-based
  listener registry, and `LocalTransactionManager` (Spring's
  `ResourcelessTransactionManager`) for transactional event semantics without a
  datasource.
- **EDA bridge** — `register_broker` / `broker()`, `publish_to_broker`, and
  `externalize_after_commit::<E>(topic, type)` (Spring Modulith event
  externalization): an in-process event published inside a committed transaction
  is forwarded to the message broker; a rolled-back one publishes nothing.
- **Declarative AOP** — `#[aspect(pointcut, order)]` with `#[before]` /
  `#[after]` / `#[after_returning]` / `#[after_throwing]` / `#[around]` advice
  markers (over the existing `firefly-aop` engine), an `inventory`-discovered
  process-global `AspectRegistry`, and the explicit `advised(...)` weave point.
- **Declarative caching** — `#[cacheable]` / `#[cache_put]` / `#[cache_evict]`
  over `async fn -> Result<V, E>`, around a process-registered cache adapter.
- **JSR-380 bean validation** — `#[derive(Validate)]`
  (`email`/`url`/`not_empty`/`length`/`range`/`pattern`/`custom`, with the
  `pattern` regex compile-checked at macro-expansion) and the `Valid<T>` axum
  extractor (422 on a constraint failure, 400 on malformed JSON).
- **Async methods** — `#[async_method]` rewrites an
  `async fn(self: Arc<Self>, …) -> R` into a non-async `fn -> TaskHandle<R>`
  spawned on a registered `TaskExecutor`.

### Changed

- The book gains an in-process-events + after-commit-externalization section
  (EDA chapter) and declarative catalogue entries for the new macros; ARCHITECTURE,
  the README, and the `transactional` / `eda` / `aop` crate READMEs document the
  new surfaces.
- Content-freshness pass: 69 confirmed documentation corrections across the book,
  top-level docs, and crate READMEs (stale counts, versions, and out-of-date code
  snippets brought in line with the code).

### Fixed

- `#[firefly(lazy)]` beans are no longer eagerly constructed during singleton
  warm-up.
- Declarative orchestration now propagates a step result-encoding failure instead
  of silently substituting null.
- Lumen's compliance endpoint answers 404 for an unknown source wallet (was 422).

## v26.6.4 — 2026-06-14

The **standalone-framework milestone**. New first-class capabilities —
config-driven auto-configuration, method security, richer declarative data
queries, and a configurable JSON mapper — land alongside a full documentation
pass that presents Firefly as the brand-new framework it is.

### Added

- **Method security** — `#[pre_authorize(...)]` (rules: `authenticated`,
  `role`, `any_role`, `authority`, `any_authority`) and
  `#[post_authorize(<expr over result/auth>)]`, backed by an ambient
  `SecurityContextHolder` (`with_authentication_scope`, `current_authentication`,
  `check_access`, `AccessRule`) that `BearerLayer` scopes automatically per
  request — so the macros work on a service method that never sees the request.
- **`@query` + `Pageable` on `#[repository]`** — `#[query("…")]` native SQL and
  `#[query(jpql = "…", entity = "…")]` custom queries (list / count / exists /
  modifying), plus a trailing `Pageable` argument for paged derived queries
  (runtime `SqlxReactiveRepository::find_by_derived_paged`).
- **`ObjectMapper`** (`firefly-web`) — a runtime JSON facade with a
  `PropertyNaming` strategy, an `Inclusion` policy, and pretty-printing, plus
  `MappingJsonConverter` to install the policy into content negotiation.
- **Config-driven auto-configuration** (DI-free, awaited at startup):
  `DataSourceProperties` + `Db::connect` / `Db::connect_with` /
  `auto_configure` (builds the pool and registers a `SqlxTransactionManager`),
  and `SecurityProperties` + `verifier_from_config` / `bearer_layer_from_config`.
- **`firefly-session-mongodb`** — a MongoDB-backed `SessionRegistry`
  (`MongoSessionRegistry`), joining the in-memory, cache-bridge, Postgres, and
  Redis session backends.
- **Application-config logging** — `log_config_from_properties` binds
  `firefly.logging.*` (root + per-logger levels, format, service, and the
  rolling file appender) straight from the main config, completing the
  configure-logging-from-application.yaml story alongside runtime
  `/actuator/loggers` control.

### Changed

- **Documentation presents Firefly as a standalone, brand-new framework.** The
  book (26 chapters plus the preface and conventions), the `docs/` set, and 74
  crate / sample / root READMEs are written in Firefly's own voice; the recurring
  "Spring parity" / "Reactor parity" callouts are now a single **Design note**.
- The default broker topology and the data-layer query metrics now live in the
  Firefly namespace — RabbitMQ defaults `firefly` / `["firefly.events"]` /
  `firefly-default`, and metrics `firefly_db_query_duration_seconds` /
  `firefly_db_queries_total` / `firefly_db_query_errors_total`.
- **Observability is auto-instrumented by default.** `Core` now installs the
  Micrometer-style HTTP server-metrics middleware (`http_server_requests_seconds`
  timer + `…_max` gauge) out of the box; opt out with
  `CoreConfig::disable_request_metrics`. The actuator already ships the
  Kubernetes liveness/readiness probes (`/actuator/health/{liveness,readiness}`),
  a Prometheus scrape target (`/actuator/prometheus`), and configurable endpoint
  exposure.

### Fixed

- **Repository reads can no longer deadlock a small connection pool.** Every
  `firefly-data-sqlx` read (derived, `@query`, and projection paths) now
  **buffers-and-releases** its pooled connection via the transaction-aware
  `*_fetch_all` helpers instead of holding it across the result stream — so a
  read never pins a connection across an `await` (the failure mode that wedged a
  one-connection SQLite pool under load).
- **Adapter connection hardening:** `cache-redis` stores a cloneable
  `MultiplexedConnection` directly (no per-call mutex serialising every command,
  and the `SCAN` loop no longer holds a lock); `eda-redis` / `session-redis`
  publish/register without holding the connection across awaits; `eda-postgres`
  / `eda-rabbitmq` claim start atomically (no auto-start connection leak) and the
  Postgres `LISTEN` channel now reconnects; `eda-kafka` moves the blocking
  `flush()` off the async executor.

### Removed

- The "Migrating from Spring Boot" appendix and the standalone migration guide.

## v26.6.3 — 2026-06-13

The **ergonomics + pluggable-persistence milestone**. Two headline wins: a
Spring-Boot-for-Rust developer experience (one `firefly` dependency, a prelude
glob, and declarative `#[derive(...)]` / `#[...]` macros) and a truly hexagonal
data layer (one set of `firefly-data` ports, real adapters for Postgres / MySQL
/ SQLite / MongoDB). Everything here is additive; the Go-parity wire contract is
unchanged. The workspace grows from 69 to **76 members** (66 → **72** framework
crates).

### Added

**Hexagonal database adapters (a new DB = a new adapter)**

- `firefly-data` — a `SqlDialect` abstraction (`PostgresDialect` /
  `MySqlDialect` / `SqliteDialect`) so the `Filter` DSL and `Specification`
  render the *same* query tree for any relational backend
  (`Filter::to_sql_with` / `Specification::to_sql_with`, with placeholder style
  `$n` vs `?`, identifier quoting, `IN`-list shape, and case-insensitive `LIKE`
  all dialect-correct). `Filter::to_sql` / `Specification::to_sql` stay the
  PostgreSQL default for back-compat. Also `Specification::to_mongo()` /
  `Filter::to_mongo()` lower the same tree to a MongoDB `$`-operator filter
  document, and the `Auditor` gains a `UserProvider` hook.
- `firefly-data-sqlx` — the **relational** repository adapter implementing the
  `firefly-data` ports over `sqlx` for **Postgres, MySQL, and SQLite** from one
  codebase: `SqlxRepository` (blocking-value) and `SqlxReactiveRepository`
  (streaming reads as a `Flux<T>`) pick the right `SqlDialect` at runtime from
  the `Db` pool's `Backend`, build dialect-aware `UPSERT`s
  (`ON CONFLICT … DO UPDATE` for Postgres/SQLite, `ON DUPLICATE KEY UPDATE` for
  MySQL), and auto-apply auditing + soft-delete. Backend-agnostic row decoding
  via `SqlxRowMapper`/`AnyRow`; writes via `ColumnValue`/`RowWriter`.
- `firefly-data-mongodb` — the **document** repository adapter over the official
  `mongodb` crate: `MongoRepository<T, ID>` implements the *same*
  `ReactiveCrudRepository` + `ReactiveSpecificationRepository` ports as the
  relational adapters, lowering `Specification::to_mongo()`, with a
  `BaseDocument` audit/soft-delete mixin and an `Audited` hook, and cursor-based
  streaming reads. A service swaps Postgres for Mongo without touching its call
  sites. All four backends are tested against **real**
  Postgres/MySQL/SQLite/MongoDB.

**Ergonomic declarative layer (one dependency, macros instead of builders)**

- `firefly-macros` — a `proc-macro` crate of derive/attribute macros (the Rust
  answer to Spring annotations / pyfly decorators): `#[derive(Command)]` /
  `#[derive(Query)]` (→ `impl firefly_cqrs::Message`, with `#[firefly(validate)]`
  / `#[firefly(cache_ttl = "…")]`); `#[command_handler]` / `#[query_handler]`
  (→ a `register_<fn>(bus)` helper); `#[derive(Component)]` /
  `#[derive(Service)]` / `#[derive(Repository)]` + the `register_all!` macro
  (→ DI-container registration); `#[scheduled]` (→ `schedule_<fn>(scheduler)`);
  `#[rest_controller]` + `#[get/post/put/delete/patch]` (→ a
  `routes(state) -> axum::Router`); `#[derive(DomainEvent)]` /
  `#[derive(AggregateRoot)]`; and `#[event_listener]`
  (→ a `subscribe_<fn>(broker)` helper).
- `firefly` — the **one-dependency facade**: `use firefly::prelude::*;` pulls in
  the whole framework (`Bus`, `Container`, `Scheduler`, `Saga`/`Step`,
  `Application`, `Core`/`CoreConfig`, `WebResult`/`WebError`/`problem_response`,
  `FireflyError`/`FireflyResult`, `Mono`/`Flux`) plus every macro. Ships
  ergonomic per-crate aliases (`firefly::cqrs`, `firefly::web`, …) and a hidden,
  stable `__rt` contract path that macro-generated code targets — so a service
  depends only on `firefly`. Heavy adapters (`data-sqlx`, `data-mongodb`,
  `eda-*`, `cache-*`, `admin`, `full`) are opt-in cargo features; a default
  build pulls in none of them.
- `samples/macro-quickstart` — `firefly-sample-macro-quickstart`, the same
  orders behaviour as the `orders` sample re-expressed declaratively over the
  single `firefly` facade: 376 source lines vs 1022 (−63%), two modules vs
  seven, with no hand-written `impl Message`, `bus.register(…)`,
  `Router::new().route(…)`, or scheduler builder.

**Distributed session registries**

- `firefly-session-redis` — `RedisSessionRegistry`, a distributed
  `firefly_session::SessionRegistry` backed by a Redis sorted set (score =
  `created_at`, oldest-first via `ZRANGE`; sliding `EXPIRE`), so the
  per-principal session-concurrency cap holds cluster-wide rather than only
  within one process.
- `firefly-session-postgres` — `PostgresSessionRegistry`, a durable, distributed
  `SessionRegistry` over a Postgres table (idempotent `ON CONFLICT` upsert,
  `ORDER BY created_at ASC` oldest-first) for relational-only deployments.

**Testkit + CLI**

- `firefly-testkit` — a `TestClient` / `TestResponse` in-process axum-router
  driver (fluent `assert_status` / `assert_json_eq` / `assert_header` / …),
  `assert_event_published` / `assert_event_published_with` over the `SpyBroker`,
  and DI test `Slice` / `BuiltSlice` helpers (the pyfly `slice_context` /
  `mock_bean` analog, with eager fail-fast resolution).
- `firefly-cli` — `completion` (shell-completion scripts), `sbom` (dependency
  SBOM), and `license` (dependency-license report) commands.

**Documentation**

- The book now renders to offline editions:
  `docs/book/dist/firefly-rust-by-example.pdf` and `.epub` (pandoc + tectonic),
  via `make book-pdf` / `make book-epub`. A new
  "Declarative Services with Macros" chapter covers the facade + macros, and the
  persistence chapter is extended with the MySQL / SQLite / MongoDB adapters.

### Fixed

- **Adversarial-review fixes** (macros + data adapters):
  - `firefly-data` — `Op::Like` / `Op::ILike` now lower to an **anchored**
    MongoDB `$regex` (`^…$`, translating SQL `%`/`_`, regex-escaping the rest),
    so the same `Specification` matches identical rows on Mongo, SQL, and
    in-memory (an unanchored Mongo `$regex` would have made `name LIKE 'A%'`
    silently match `"bAr"`).
  - `firefly-data-sqlx` — `save` resurrects soft-deleted rows (clears
    `deleted_at` on upsert); timestamp coercion is tag-driven, so
    RFC3339-looking text is no longer mis-typed as a timestamp.
  - `firefly-macros` — `#[derive(DomainEvent)]` JSON-encodes through the facade's
    `__rt::serde_json` (preserving the one-dependency contract);
    `#[event_listener]` preserves the consumer `group` when given a positional
    topic; `#[scheduled]` rejects `cron` + `initial_delay` with a compile error.
- **`serde_json` ordering wire-parity** — linking the `mongodb`/`bson` crate
  turned on `serde_json/preserve_order` workspace-wide (Cargo feature
  unification), flipping `serde_json::Map` from sorted-key to insertion-order;
  restored deterministic sorted-key wire output where it is contractually
  required (`config-server`, `openapi`, `callbacks`).
- Stabilized flaky admin SSE timing tests (raised the under-load timeout).

## v26.6.2 — 2026-06-13

The **reactive milestone**. This release adds a WebFlux-style reactive
core and threads it through the framework, makes every vendor adapter
real (no stubs remain), introduces real-infrastructure Docker testing
and an mdBook documentation site, and ships the `firefly` developer CLI
and an end-to-end reactive sample. The Go-parity wire contract is
unchanged; everything here is additive.

### Added

**Reactive core (the keystone)**

- `firefly-reactive` — a faithful Project Reactor / WebFlux analog:
  `Mono<T>` (0-or-1 + error) and `Flux<T>` (0..N + terminal error) over
  `tokio` futures/streams, fixed to `firefly_kernel::FireflyError`. Ships
  a `Scheduler` (`Immediate` / `Parallel` / `BoundedElastic`), a
  `FluxSink` for imperative emission (`Flux::create`), a `Backoff` retry
  policy, and the full operator surface — transform (`map` / `flat_map` /
  `concat_map` / `scan`), combine (`merge` / `concat` / `zip` /
  `combine_latest`), reduce/terminal (`reduce` / `collect_list` /
  `collect_map`), error (`on_error_resume` / `on_error_continue` /
  `retry` / `retry_backoff`), time (`timeout` / `debounce` / `sample` /
  `interval`), backpressure (`on_backpressure_{buffer,drop,latest}` /
  `limit_rate`), and windowing (`buffer` / `window` / `group_by`).

**Reactive integration across the framework**

- `firefly-web` — reactive HTTP responders: `MonoJson<T>` (renders a
  `Mono` as JSON, `Ok(None)` → 404 problem+json, `Err` → RFC 7807),
  `NdJson<T>` and `Sse<T>` (stream a `Flux` as `application/x-ndjson` /
  `text/event-stream` with **true backpressure** — never buffered),
  and `SseEvents` (pre-built `firefly_sse::Event` frames).
- `firefly-data` — the reactive `ReactiveCrudRepository<T, ID>` (with
  `find_all` / `find_by_id` / `save` / `delete_by_id` / `count` returning
  `Mono`/`Flux`), an in-memory `ReactiveMemoryRepository`, a
  `ReactiveSpecificationRepository`, and a real `PostgresReactiveRepository`
  that streams rows out of `find_all()` as a `Flux<T>` over
  `tokio-postgres` (with `RowMapper` / `TableConfig`).
- `firefly-client` — the reactive `WebClient` (`WebClientBuilder` →
  `get`/`post`/`put`/`delete`/`patch` → `RequestSpec` →
  `retrieve()` → `ResponseSpec::body_to_mono::<T>()` /
  `body_to_flux::<T>()` / `exchange()`), the Rust analog of WebFlux's
  `WebClient`.
- `firefly-eda` — reactive subscription: `InMemoryBroker::subscribe_reactive`
  (and `_with_buffer`) yields a `Flux<Event>` with bounded backpressure,
  and `publish_mono` is a cold reactive publish.
- `firefly-cqrs` — reactive bus: `Bus::send_mono` / `query_mono` (and the
  `_with_context` variants) wrap dispatch in a lazy `Mono<R>`, running the
  same handler lookup and validation/authorization/caching middleware;
  `cqrs_error_to_firefly` maps `CqrsError` onto the right HTTP status.

**Real vendor adapters — zero stubs**

- The SendGrid and Resend email channels are now real: `SendGridEmailProvider`
  POSTs to SendGrid v3 `/mail/send`, `ResendEmailProvider` POSTs to Resend
  `/emails`, both over `reqwest`; their Go-parity envelope `Channel`s
  delegate to the real provider. No notification, IDP, or ECM adapter
  ships a `NotImplemented` sentinel any longer.
- `firefly-cache-postgres` is a real `cache::Adapter` (`PostgresCacheAdapter`)
  backed by a Postgres key/value table with TTL over `tokio-postgres`
  (upsert, `set_if_absent`, `delete_prefix`, key scan, health check).
- `firefly-starter-web` is a real web-stack starter: `WebStack` layers
  `Core` with CORS, security headers, request metrics, and an access log
  by default, with optional `FilterChain` security.

**Real-infrastructure testing**

- A `docker-compose.yml` stack (Postgres, Redis, RabbitMQ, Redpanda,
  Keycloak, LocalStack S3, Azurite Blob, MailHog SMTP) plus
  `make infra-up` / `make test-integration` / `make infra-down`. The
  env-gated integration tests run the cache, EDA, IDP, ECM, notification,
  and reactive-Postgres adapters — and the reactive-banking sample —
  against the **real** services, while `cargo test --workspace` stays
  green offline (each test skips when its connection env var is unset).

**Documentation, tooling, and samples**

- `docs/book` — an mdBook guide (builds with mdBook) covering why-Firefly,
  quickstart, configuration, dependency wiring, the keystone reactive
  model, HTTP APIs, persistence, DDD, CQRS, EDA, event sourcing, sagas,
  HTTP clients, security, observability, scheduling/notifications,
  caching, testing, the CLI, production, and appendices (Spring mapping,
  module index, glossary).
- `firefly-cli` — the `firefly` developer binary (`new`, `generate`/`g`,
  `info`, `doctor`, `db`, `openapi`, and remote actuator introspection),
  installable via `make cli-install` / `cargo install --path crates/cli`.
- `samples/reactive-banking` — `firefly-sample-reactive-banking`, an
  end-to-end reactive service: reactive CQRS, event sourcing, a
  saga-backed money transfer, a `Flux<AccountEvent>` NDJSON/SSE stream,
  JWT-secured `starter-web`, and a `WebClient` SDK, running on in-memory
  defaults or real Postgres/Kafka.

### Changed

- Every source file now carries the Apache 2.0 license header (Firefly
  Software Foundation, 2026).
- Documentation refreshed end to end (README, `MODULES.md`, the `docs/`
  guides, and the book): the reactive core and integrations are now
  prominent, all vendor adapters are documented as real/Full, the
  real-infra testing path is described, and the workspace count is
  current (66 framework crates; 69 workspace members).

### Fixed

- Adversarial-review fixes across the reactive surfaces and adapters
  (error mapping, backpressure/termination semantics, and connection
  handling), and corrected documentation that previously described
  SendGrid/Resend, `cache-postgres`, and `starter-web` as port-pending
  stubs.

## v26.6.1 — 2026-06-12

**First public release** of the Rust port at
<https://github.com/fireflyframework/fireflyframework-rust>.

Fourth sibling port of the Java/Spring Boot Firefly Framework, joining
the .NET, Go, and Python (PyFly) ports. Ported with full module parity
against the Go port (the canonical compiled-language reference) **plus a
purely additive PyFly-parity layer**: one Cargo workspace with 67
members — 65 `firefly-*` crates under `crates/`, the cross-crate
integration suite, and the Orders reference sample. Targets Rust 1.85+
(edition 2021) on the tokio + axum + serde stack, with `thiserror`
errors, `async-trait` ports, RustCrypto primitives, and `tracing`
structured logging. Wire-compatible with the sibling ports: RFC 7807
`application/problem+json`, `X-Correlation-Id` propagation,
`Idempotency-Key` semantics, event envelope JSON, HMAC webhook
signatures, Spring-Cloud-Config response shape, and `V###__name.sql`
migration naming.

The Go-parity core (foundational, platform, starter tiers) is kept
byte-stable on the wire; everything in the **PyFly-parity layer** below
layers onto the existing crates without changing any established wire
format.

### Added

**Foundational tier (6 crates)**

- `firefly-kernel` — RFC 7807 `ProblemDetail`, `FireflyResult<T>`,
  `Clock`, `FireflyError` hierarchy, task-local correlation scopes
- `firefly-utils` — try/retry helpers with backoff, slug, AES-256-GCM,
  templates
- `firefly-validators` — IBAN, BIC, Luhn, currency, phone, password,
  sort code, VAT, Spanish IDs
- `firefly-web` — problem renderer, correlation, idempotency, PII
  masking as composable `tower` layers
- `firefly-config` — typed YAML / env / flag binding with profile
  selection
- `firefly-i18n` — locale-aware message bundles + Accept-Language
  resolver

**Platform tier (19 crates)**

- `firefly-cache`, `firefly-observability`, `firefly-data`,
  `firefly-cqrs`, `firefly-eda` (in-memory broker full; Kafka/RabbitMQ
  scaffolds return typed sentinels), `firefly-eventsourcing`,
  `firefly-orchestration` (Saga / Workflow DAG / TCC),
  `firefly-rule-engine`, `firefly-plugins`, `firefly-lifecycle`,
  `firefly-actuator`
  (`/actuator/{health,info,metrics,env,tasks,version}`),
  `firefly-scheduling`, `firefly-resilience`, `firefly-security`,
  `firefly-migrations`, `firefly-openapi`, `firefly-sse`,
  `firefly-transactional`, `firefly-testkit`

**Adapter tier**

- Full: `firefly-client` (REST builder; SOAP/gRPC/WS scaffolds),
  `firefly-config-server`, `firefly-idp` + `firefly-idp-internal-db`,
  `firefly-ecm` (port + LocalStore), `firefly-notifications`
  (dispatcher + memory channel), `firefly-callbacks`,
  `firefly-webhooks`
- Real vendor adapters (PyFly-parity): `firefly-idp-keycloak`
  (OIDC + admin REST), `firefly-idp-azure-ad` (Microsoft Graph + ROPC),
  `firefly-idp-aws-cognito` (JSON API + self-contained SigV4),
  `firefly-ecm-storage-aws` (S3), `firefly-ecm-storage-azure`
  (Blob Storage), `firefly-ecm-esignature-docusign` (REST v2.1),
  `firefly-ecm-esignature-adobe-sign` (REST v6),
  `firefly-ecm-esignature-logalty` (eIDAS REST),
  `firefly-notifications-twilio` (SMS), `firefly-notifications-firebase`
  (FCM push) — each keeps a Go-parity/back-compat stub alongside the
  real provider
- Stub (port-asserting, typed not-implemented errors):
  `firefly-notifications-sendgrid`, `firefly-notifications-resend`

**Starter tier (5 crates)**

- `firefly-starter-core` (one-call `Core::new(CoreConfig)` wiring),
  `firefly-starter-application`, `firefly-starter-domain`,
  `firefly-starter-data`, `firefly-backoffice`

**PyFly-parity layer**

New cross-cutting crates (opt-in; the Go-parity core does not depend on
them):

- `firefly-container` — opt-in `TypeId`-keyed DI container (service
  locator): `register_factory` / `resolve` / `resolve_all` /
  `bind::<dyn Trait>` / `Scope` / `Provider<T>` / `RefreshScope`;
  explicit factory closures (no reflective autowiring)
- `firefly-aop` — Spring-style aspect advice: `Pointcut` glob matcher,
  `JoinPoint`, `Aspect` (before / around / after-returning /
  after-throwing / after), `AspectRegistry`, `intercept` chain executor
  with explicit weaving at the call site
- `firefly-session` — server-side HTTP `Session` (typed serde
  attributes), `SessionStore` (`MemorySessionStore` / `CacheSessionStore`),
  `SessionLayer` (cookie load/save, id rotation, invalidation, HMAC
  signing), `SessionRegistry` + concurrency control
- `firefly-shell` — Spring-Shell-style CLI framework: `CommandSpec`
  builder, typed `CommandArgs`, `StdShell` parser + REPL,
  `ApplicationArguments`, `CommandLineRunner` / `ApplicationRunner` +
  `RunnerRegistry`
- `firefly-websocket` — WebSocket server over axum: `WsSession`,
  `WebSocketHandler`, `ws_route` / `serve_ws`, topic `BroadcastHub`
- `firefly-cli` — the `firefly` developer binary: `new`, `generate`/`g`,
  `info`, `doctor`, `actuator`
- `firefly-admin` — Spring-Boot-Admin-style embedded dashboard (SPA +
  JSON API over `firefly-actuator` + SSE live streams + instance
  registry / client modes; `firefly.admin.*` config)

Real infrastructure transport / cache adapters (implement the existing
platform ports; pull their backing SDK only when selected):

- `firefly-cache-redis` — `cache::Adapter` over Redis (RESP via `redis`)
- `firefly-eda-kafka` — `eda::Broker` over Apache Kafka (`rdkafka`)
- `firefly-eda-rabbitmq` — `eda::Broker` over RabbitMQ (`lapin`,
  durable direct exchange, publisher confirms)
- `firefly-eda-postgres` — `eda::Broker` as a Postgres transactional
  outbox + `LISTEN`/`NOTIFY` (`tokio-postgres`, advisory-lock drain)
- `firefly-eda-redis` — `eda::Broker` over Redis Streams consumer groups
- `firefly-notifications-smtp` — SMTP email channel over `lettre`
  (real MIME, STARTTLS, BCC-not-leaked)

Reserved as port-pending placeholders for the next wave (compile and
carry their locked dependency set; implementation lands without
disturbing the wire contract):

- `firefly-cache-postgres` — Postgres-backed `cache::Adapter` (key/value
  table with TTL over `tokio-postgres`)
- `firefly-starter-web` — web-stack starter bundling `starter-core` +
  web middleware + security + actuator wiring

Additive extensions to existing crates (every Go-parity wire format
unchanged):

- `firefly-web` — CORS, security headers, CSRF (double-submit cookie),
  request access log, HTTP server metrics, extended correlation
  (`X-Request-Id` / `X-Tenant-Id` / `traceparent`), content negotiation
  (JSON/XML), and a `server.*` bootstrap (`ServerProperties` / TLS)
- `firefly-security` — JWKS resource-server `Verifier`, `oauth2`
  (client registrations + login flow with PKCE/OIDC + authorization
  server), `RoleHierarchy`, `guards`, `CsrfLayer`, and persistent token
  stores (in-memory / Redis / Postgres)
- `firefly-observability` — labeled metrics with `timed`/`counted`,
  Prometheus text exposition, and native W3C trace-context propagation
- `firefly-actuator` — Spring-Boot management model: liveness/readiness
  probes, health groups, runtime loggers, scheduled tasks, caches,
  `/actuator/refresh`, `httpexchanges`, Micrometer metric detail,
  Prometheus, custom endpoints, and the `management.endpoints.web`
  exposure model
- `firefly-config` — `${key:default}` / `${ENV}` placeholder
  resolution, runtime reload (`ReloadableConfig` / `Refresher` →
  `/actuator/refresh`), masked property-source introspection,
  multi-profile overlays, and a Spring-Cloud-Config client
- `firefly-orchestration` — workflow step compensation
  (`Node::with_compensation`, reverse-order rollback), `wait_all` /
  `wait_any` join points (`WaitTarget`), child workflows
  (`ChildWorkflowService`), continue-as-new (`ContinueAsNew`),
  conditional + async steps, per-step retry / backoff / timeout
  (`invoke_with_policy`), inter-step data passing (`StepContext`),
  durable execution state, stuck-run recovery, a dead-letter queue,
  signal / timer workflow nodes, an `EventGateway` for broker-driven and
  scheduled saga starts, a ruleset-style `validator`, and a REST admin
  surface (`MemoryPersistence` / `SqlitePersistence` adapters)
- `firefly-eventsourcing` — global cross-aggregate `EventStore::stream_all`
  + cross-aggregate projections, multi-tenancy (tenant-scoped append /
  load / stream), and an `EventSourcedRepository`
- `firefly-rule-engine` — `between` / null / `regex` operators,
  `Rule.otherwise`, `EvaluationMode` (All / FirstMatch), a ruleset
  validator, and pluggable `ActionHandler`s
- `firefly-data` — `Mapper` / `Mapping` / `Projection` object mapper,
  a derived-query parser (`QueryMethodParser` / `ParsedQuery`), and
  `Pageable` / `Sort` / `Order` paging requests
- `firefly-validators` — `national_id` and `tax_id` validators
- `firefly-kernel` — a `ddd` module (`Entity`, `Specification`
  combinators, domain events / `PendingEvents`), task-local request and
  tenant scopes alongside correlation, and a typed `ErrorResponse`
  (`ErrorCategory` / `ErrorSeverity` / `FieldError`)
- `firefly-eda` — `Event.key` routing key, glob topic subscriptions,
  round-robin consumer groups, `EventFilter` chains
  (`HeaderEventFilter` / `PredicateEventFilter`), a queryable
  `EdaDeadLetterStore`, an `EventPublisherHealthIndicator`, and a
  `wrap_listener` retry/DLQ wrapper
- `firefly-cache` — LRU eviction + hit/miss statistics on the in-process
  `MemoryAdapter`

**Tests + samples**

- `tests/integration` — cross-crate suite (CQRS roundtrip, callbacks
  dispatch with HMAC verification by webhooks, saga compensation,
  starter-core boot)
- `samples/orders` — Orders reference service (`firefly-sample-orders`)

**Documentation + tooling**

- Per-crate `README.md` (overview, public surface, quick start),
  cross-linked from `MODULES.md` and the root `README.md`
- `docs/ARCHITECTURE.md`, `docs/CONFIGURATION.md`,
  `docs/MIGRATION-GUIDE.md`, `docs/DESIGN.md`
- `Makefile` with cargo-based `build` / `test` / `clippy` / `fmt-check`
  / `sample` / `ci` targets; canonical version via `Makefile.VERSION` +
  `firefly_kernel::VERSION`

### Quality gate

`make ci` = `cargo fmt --all --check` +
`cargo clippy --workspace --all-targets -- -D warnings` +
`cargo build --workspace` + `cargo test --workspace`.
