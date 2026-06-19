# Spring Security Parity

This appendix maps Firefly's security tier onto **Spring Security 6 / Spring
Boot 3**: what is supported today, the Spring-faithful behaviours you should
know about, and the roadmap for the rest. It complements the
[Security](./14-security.md) chapter, which is the hands-on tutorial.

Firefly's security tier is an idiomatic Rust port — `tower` layers instead of
servlet filters, traits instead of interfaces, builder functions instead of the
`HttpSecurity` DSL — so *parity is semantic, not literal*. A feature is
"present" when it delivers Spring's behaviour, regardless of shape.

## Coverage at a glance

In the **Status** column, :status-supported: marks a supported feature,
:status-partial: a supported but opt-in (feature-gated) module, and
:status-planned: a roadmap item.

| Area | Status | Notes |
|------|--------|-------|
| HTTP request authorization (`FilterChain`, RBAC, role hierarchy) | :status-supported: | Path-segment-aware matching, deny-by-default, first-match-wins |
| Bearer / OAuth2 resource server (JWT) | :status-supported: | JWKS with RSA + **EC (ES256/384)** + **EdDSA**; `iss`/`aud`/`exp`/`nbf` validation; 60 s clock-skew leeway; RFC 6750 `WWW-Authenticate` challenge |
| Symmetric JWT (`JwtService`) | :status-supported: | HS256/384/512, `exp` required, clock-skew leeway |
| Method security (`#[pre_authorize]` / `#[post_authorize]`) | :status-supported: | Works uniformly across **bearer *and* session/OAuth2-login** auth; keyword rules **and** SpEL-style expressions over arguments + principal |
| Method-security depth (`@PreFilter`/`@PostFilter`, `PermissionEvaluator`) | :status-supported: | `#[pre_filter]` / `#[post_filter]` collection filtering; `PermissionEvaluator` + `has_permission` (`hasPermission(...)`), usable inside the expression forms |
| Role checks (`hasRole`) | :status-supported: | Accepts Spring's `ROLE_` prefix *and* bare role names |
| CORS | :status-supported: | Rejects the unsafe wildcard-origin + credentials combination |
| Security response headers | :status-supported: | HSTS, CSP, X-Frame-Options, X-Content-Type-Options, Referrer-Policy, Permissions-Policy; **HSTS is secure-request-only** by default |
| CSRF (double-submit cookie) | :status-supported: | `Secure` cookie follows the request scheme; Bearer bypass |
| Session management | :status-supported: | Fixation rotation, concurrency control, distributed registries (Redis / **Postgres, with TTL pruning** / Mongo) |
| Password encoding | :status-supported: | BCrypt + Argon2id; constant-time login (no user-enumeration timing oracle) |
| OAuth2 / OIDC login | :status-supported: | Auth-code + PKCE + state/nonce; **`id_token` is always validated** (never silently skipped) |
| One-time-token login (magic link) | :status-supported: | Spring 6.4 `oneTimeTokenLogin()` — `OneTimeTokenService` + delivery handler + `/ott/generate` + `/login/ott` |
| WebAuthn / passkeys | :status-partial: | Spring 6.4 `webAuthn()` — feature-gated `webauthn` module (registration + authentication ceremonies) |
| IdP adapters | :status-supported: | Internal-DB, Keycloak, Azure AD / Entra, AWS Cognito |
| Authentication architecture | :status-supported: | `AuthenticationManager`/`ProviderManager`/`AuthenticationProvider`, `UserDetails`+`DaoAuthenticationProvider`, `SecurityContextRepository`, `AuthenticationEventPublisher`, pluggable `AuthenticationEntryPoint`/`AccessDeniedHandler` |
| Delegating password encoder (`{id}` migration) | :status-supported: | `DelegatingPasswordEncoder` (`{bcrypt}`/`{argon2}`/`{noop}`) with `upgrade_encoding` re-hash-on-login |
| HTTP Basic (`httpBasic()`) | :status-supported: | `HttpBasicLayer` over the auth spine; absent header passes through, invalid/malformed → `401` + `WWW-Authenticate: Basic realm=…` |
| Form login (`formLogin()`) | :status-supported: | `form_login_routes` (`POST /login`), session-id rotation (anti-fixation), pluggable success/failure handlers, saved-request-aware redirect |
| Remember-me (`rememberMe()`) | :status-supported: | `TokenBasedRememberMeServices` — signed, expiring, password-hash-bound token; `is_remembered()` / `is_fully_authenticated()` trust levels |
| `RequestCache` / `SavedRequest` | :status-supported: | `HttpSessionRequestCache` — the pre-login page restored after authentication (same-origin redirect only) |
| `SessionCreationPolicy` | :status-supported: | `Always`/`IfRequired`/`Never`/`Stateless`; `Stateless` installs the null context repository for token APIs |
| Multiple filter chains | :status-supported: | `SecurityFilterChains` — first matching `RequestMatcher` wins (Spring's `FilterChainProxy`) |
| Outbound OAuth2 client (`AuthorizedClientManager`) | :status-supported: | `OAuth2AuthorizedClientManager` + `OAuth2AuthorizedClientService` — client-credentials / refresh-token grants, token cache + auto-refresh for downstream calls |
| Opaque-token introspection (RFC 7662) | :status-supported: | `RemoteTokenIntrospector` (`OpaqueTokenIntrospector`) — a drop-in resource-server `Verifier` |
| RP-initiated logout (OIDC) | :status-supported: | `oidc_logout_url` — logout redirects to the provider `end_session_endpoint` (`OidcClientInitiatedLogoutSuccessHandler`) |
| Authorization server | :status-partial: | `AuthorizationServer` (client-credentials + refresh-token) mounted via `AuthorizationServerRouter` (`/oauth2/token`, RFC 8414 metadata); server-side authorization_code grant on the roadmap |
| LDAP / Active Directory authentication | :status-partial: | Feature-gated `ldap`: `LdapAuthenticationProvider` (bind auth + group authorities) + `ActiveDirectoryLdapAuthenticationProvider`, over `ldap3` (`ldapAuthentication()`) |
| ACL / domain-object security | :status-supported: | `Acl` / `AccessControlEntry` / `Permission` / `Sid` / `ObjectIdentity`, `AclService` + `InMemoryAclService`, and `AclPermissionEvaluator` wiring `hasPermission(...)` to per-object ACLs (`spring-security-acl`) |
| SAML2 (`saml2Login()`) | :status-partial: | Feature-gated `saml2`: SP-side `RelyingPartyRegistration`, SP-initiated `AuthnRequest` redirect, and signed-response verification (`OpenSaml4AuthenticationProvider`) with one-time-use replay, over `samael` (SLO / signed-AuthnRequest / encrypted-assertions remain) |

## Spring-faithful behaviours to know

These match Spring Security 6 defaults and may differ from a naïve port — each
has a configuration escape hatch:

- **`hasRole('ADMIN')` matches the authority `ROLE_ADMIN`.** A ported Spring or
  JWT principal carrying `ROLE_`-prefixed authorities authorizes without you
  hand-stripping prefixes; bare role names keep working too.
- **Method security works behind every authentication mechanism.** A
  session-authenticated or OAuth2-login user satisfies `#[pre_authorize]` /
  `current_authentication()`, not only a bearer-token caller.
- **HSTS is sent only over secure requests** (`HstsHeaderWriter` default).
  Configure `hsts_include_insecure` to force it.
- **The CSRF cookie is `Secure` only when the request is secure**, so the
  double-submit pair also works over plain-HTTP local development.
- **A wildcard CORS origin combined with credentials is rejected** at
  construction (`CorsLayer::try_new` returns an error) — use explicit origins.
- **JWT/JWKS validation tolerates 60 s of clock skew** and validates `nbf`;
  EC and EdDSA JWKS keys verify, not just RSA.
- **An OIDC `id_token` is never trusted without validation** — if it cannot be
  verified the login fails rather than falling through to userinfo.
- **Path-prefix authorization rules are segment-aware**: `permit("/api")`
  matches `/api` and `/api/...` but not `/api-internal`.
- **Unknown-username login spends comparable bcrypt time** to a wrong password,
  closing the user-enumeration timing oracle.

## Form login, HTTP Basic, and remember-me

The classic web authentication mechanisms, faithful to Spring's defaults:

- **HTTP Basic** — `HttpBasicLayer::new(manager)` reads
  `Authorization: Basic …` and authenticates through the Tier 1
  `AuthenticationManager`. An **absent** header passes through (so a session or
  bearer layer can take over); an **invalid or malformed** one is rejected with
  `401` and a `WWW-Authenticate: Basic realm="…"` challenge — Spring's
  `BasicAuthenticationFilter`.
- **Form login** — `form_login_routes(state)` mounts `POST /login`
  (url-encoded `username` + `password`), rotates the session id on success
  (anti-fixation) **before** persisting the context, then redirects. The
  success/failure responses are swappable (`FormLoginSuccessHandler` /
  `FormLoginFailureHandler`), and the success path is **saved-request-aware**.
- **Remember-me** — `TokenBasedRememberMeServices` mints a signed, expiring
  cookie token bound to the user's stored password hash and a server key
  (Spring's `TokenBasedRememberMeServices`): a password change, a clock past the
  expiry, a tampered token, or the wrong key all reject. A remembered context is
  *authenticated but not fully authenticated* — `is_remembered()` is `true` and
  `is_fully_authenticated()` is `false`, so a sensitive route can demand a fresh
  login (Spring's `isFullyAuthenticated()`).
- **Request cache** — when the entry point sends an unauthenticated user to log
  in, `HttpSessionRequestCache` remembers the page they wanted; form login then
  returns them there instead of the default target (Spring's
  `SavedRequestAwareAuthenticationSuccessHandler`). Only **same-origin** targets
  are honoured — a saved path is rejected if it could redirect off-site.
- **Session creation policy** — `SessionCreationPolicy::{Always, IfRequired,
  Never, Stateless}` chooses whether the security tier persists its context in
  the session; `Stateless` (token APIs) installs the null context repository.
- **Multiple filter chains** — `SecurityFilterChains` routes each request to the
  first chain whose `RequestMatcher` (e.g. `PathRequestMatcher::new("/api")`)
  matches, so a locked-down `/api/**` and a permissive web surface coexist —
  Spring's `FilterChainProxy`.

## Method security

`#[pre_authorize]` / `#[post_authorize]` guard a service method against the
ambient principal — no `Request` in the signature. Beyond fixed keyword rules
(`role = "ADMIN"`, `any_authority = [..]`), they accept **expressions**, the
Rust analog of Spring's SpEL:

- **Argument + principal binding** — a non-keyword `#[pre_authorize(...)]` is a
  boolean Rust expression evaluated *before* the body with the method's
  parameters and `auth` (a `&Authentication`) in scope:
  `#[pre_authorize(auth.has_role("ADMIN") || auth.principal == owner)]`
  (Spring's `@PreAuthorize("#owner == authentication.name")`). `#[post_authorize]`
  binds `result` + `auth` over the return value.
- **`PermissionEvaluator`** — register one process-wide with
  `set_permission_evaluator`, then call `has_permission(auth, target, permission)`
  inside any pre/post expression (Spring's `hasPermission(#obj, 'read')`). With
  no evaluator registered, every permission is **denied** (fail-closed).
- **`#[pre_filter]` / `#[post_filter]`** — filter a collection by a per-element
  predicate: `#[post_filter(element.owner == auth.principal)]` drops the rows the
  caller doesn't own from the returned `Vec`; `#[pre_filter(items, …)]` does the
  same to a `mut` argument before the body (Spring's `@PreFilter`/`@PostFilter`,
  where `element` is the `filterObject`).

All four fail closed: no ambient context denies with `Unauthenticated`, a false
expression with `Forbidden`.

## OAuth2 ecosystem

Beyond the browser login flow (auth-code + PKCE + OIDC), Firefly covers the
wider OAuth2 ecosystem:

- **Opaque-token introspection (RFC 7662)** — `RemoteTokenIntrospector`
  (Spring's `OpaqueTokenIntrospector`) validates non-JWT bearer tokens against
  the authorization server's `/introspect` endpoint and maps the `active`
  response to an `Authentication`. It implements `Verifier`, so it drops into a
  `BearerLayer` as an alternative to local JWT verification. Fails closed.
- **Outbound client (`AuthorizedClientManager`)** —
  `OAuth2AuthorizedClientManager` + `OAuth2AuthorizedClientService` obtain,
  **cache**, and **auto-refresh** the access tokens the app needs to call
  downstream services (client-credentials for service-to-service, refresh-token
  for delegated calls), reusing a token until it nears expiry.
- **RP-initiated logout (OIDC)** — when the login provider advertises an
  `end_session_endpoint`, `POST /logout` redirects the browser there with an
  `id_token_hint` + `post_logout_redirect_uri` so the session ends at the IdP
  too (Spring's `OidcClientInitiatedLogoutSuccessHandler`).
- **Authorization server** — `AuthorizationServer` (client-credentials +
  refresh-token, HS256) is mounted over HTTP by `AuthorizationServerRouter`:
  `POST /oauth2/token` (RFC 6749) and `GET /.well-known/oauth-authorization-server`
  (RFC 8414 metadata). The server-side authorization_code grant is a follow-up.

## Passwordless login

Firefly ships the two Spring Security 6.4 passwordless mechanisms:

- **One-time token (magic link)** — `ott_login_routes` exposes
  `POST /ott/generate` (mints a single-use, expiring token and hands it to your
  delivery handler) and `GET /login/ott?token=…` (redeems it, rotates the
  session, and establishes the security context). The default handler logs only
  that a token was issued — wire a real email/SMS handler in production.
- **WebAuthn / passkeys** — the feature-gated `webauthn` module provides the
  registration and authentication ceremonies (`/webauthn/register/options`,
  `/webauthn/register`, `/webauthn/authenticate/options`, `/login/webauthn`)
  built on `webauthn-rs`, storing credentials through a pluggable repository.

## LDAP / Active Directory

The feature-gated `ldap` module (opt-in: `--features ldap`, pulls in `ldap3`)
authenticates username/password credentials against a directory — Spring's
`ldapAuthentication()`. Both providers are
[`AuthenticationProvider`](#)s, so they plug straight into the Tier 1
`ProviderManager`:

- **`LdapAuthenticationProvider`** — **bind authentication**: search the user's
  DN under a base with a filter (`(uid={0})`, the username RFC 4515-escaped),
  bind as that DN with the password (the directory verifies it), then map group
  membership (`(member={0})`) to `ROLE_<GROUP>` authorities (Spring's
  `BindAuthenticator` + `DefaultLdapAuthoritiesPopulator`).
- **`ActiveDirectoryLdapAuthenticationProvider`** — binds as the
  `userPrincipalName` (`user@domain`) and maps the user's `memberOf` groups to
  roles (Spring's `ActiveDirectoryLdapAuthenticationProvider`).

The LDAP wire operations sit behind an `LdapOperations` port (real adapter:
`Ldap3Operations`), so the provider logic is unit-tested without a live
directory. Safety behaviours, Spring-faithful and verified by a pre-release
adversarial review:

- An **empty password is rejected before binding** — a simple bind with an empty
  password is an anonymous bind that most directories accept (an authentication
  bypass).
- The username/DN is **RFC 4515-escaped** in every filter (LDAP-injection safe),
  and unknown-user / wrong-password return the **same error value**.
- An **ambiguous user search** (more than one matching entry) is rejected rather
  than binding against an arbitrary first match — Spring's
  `IncorrectResultSizeDataAccessException`.
- A **directory error while populating authorities** fails the login instead of
  silently authenticating with no roles, and a **malformed directory entry** is
  turned into a clean error rather than aborting the request.

## Domain-object security (ACL)

Where the [`PermissionEvaluator`](#) answers "may this principal do X to this
object?" with arbitrary code, an **ACL** answers it from per-object
access-control lists — the Rust analog of `spring-security-acl`. It is pure Rust
(no extra dependencies):

- **`Permission`** — the `BasePermission` bitmask (`READ`, `WRITE`, `CREATE`,
  `DELETE`, `ADMINISTRATION`), combinable into a cumulative mask.
- **`Sid`** — a security identity: a `Principal` (username) or an `Authority`
  (role), Spring's `PrincipalSid` / `GrantedAuthoritySid`.
- **`ObjectIdentity`** — a domain object's `(type, identifier)` key.
- **`Acl`** — an owner plus ordered **`AccessControlEntry`s** (grant or deny a
  permission to a sid) plus an optional parent for **inheritance**.
- **`AclService`** / **`InMemoryAclService`** — look an ACL up by identity
  (Spring's `MutableAclService`).
- **`AclPermissionEvaluator`** — wires an `AclService` into the method-security
  `hasPermission(...)` expression, by both object reference and `(type, id)`.

Evaluation is **default-deny**: a permission is granted only when an applicable
*granting* entry is found locally or up the inheritance chain; the **first entry
matching a `(sid, permission)` wins**, so a deny placed before a grant takes
precedence (Spring's `DefaultPermissionGrantingStrategy`). The inheritance walk
is bounded, so a cyclic or pathologically deep parent chain terminates (and
denies) rather than looping.

## SAML2 single sign-on

The feature-gated `saml2` module (opt-in: `--features saml2`) is the
Service-Provider side of the SAML 2.0 Web-Browser-SSO profile — Spring's
`saml2Login()`. The XML-signature verification, canonicalization, and SAML
profile checks (audience, recipient, `InResponseTo`, status, time conditions)
are delegated to the [`samael`] crate (which links the battle-tested
`xmlsec`/`libxml2`/OpenSSL stack); this module is the Spring-faithful, hardened
wrapper:

- **`RelyingPartyRegistration`** (+ `InMemoryRelyingPartyRegistrationRepository`)
  — one SP↔IdP relationship, configured from IdP metadata or explicit
  asserting-party details (Spring's `RelyingPartyRegistration`).
- **SP-initiated `AuthnRequest`** — `authn_request_redirect` builds the
  HTTP-Redirect-binding URL and returns the request `ID` to remember
  (`Saml2AuthenticationRequestRepository`).
- **`authenticate`** — verifies a POST-binding response and maps the `NameID`
  (and configured attributes) to an `Authentication` (Spring's
  `OpenSaml4AuthenticationProvider`).
- **SP metadata** — `metadata_xml` (Spring's `Saml2MetadataFilter`).

Hardening on top of `samael`:

- **Fail-closed on a missing IdP signing certificate** — without one `samael`
  would skip signature verification entirely (an authentication bypass), so
  building a registration refuses it.
- **Signature-algorithm allow-list** pinned to SHA-256+ RSA/ECDSA (`samael`
  otherwise accepts *all* algorithms — an algorithm-substitution risk).
- **One-time-use replay protection** (`AssertionReplayCache`) — the SAML
  profile requires it but `samael` does not track it.
- All native XML-Security calls are **serialized** (the stack is not
  concurrency-safe).

Single-logout, signed `AuthnRequest`s, and encrypted assertions remain on the
roadmap. (`saml2` links a system `libxml2` + `xmlsec1` + OpenSSL; the default
build is unaffected.)

[`samael`]: https://crates.io/crates/samael

## Roadmap

Parity is delivered in tiers, each its own increment:

1. **Hardening (done)** — the Spring-faithful behaviours above.
2. **Authentication spine (done)** — `AuthenticationManager` / `ProviderManager`,
   `DaoAuthenticationProvider` + `UserDetails`, `SecurityContextRepository`,
   `DelegatingPasswordEncoder`, authentication events, pluggable entry-point /
   access-denied handlers.
3. **Web mechanisms (done)** — form login, HTTP Basic, remember-me,
   `RequestCache` / `SavedRequest`, `SessionCreationPolicy`, multiple filter
   chains.
4. **Method-security depth (done)** — SpEL-style argument/principal binding,
   `@PreFilter`/`@PostFilter`, `PermissionEvaluator`.
5. **OAuth2 ecosystem (done)** — opaque-token introspection (RFC 7662), the
   outbound authorized-client manager, RP-initiated logout, and the
   authorization server mounted over HTTP with RFC 8414 metadata. (The
   server-side authorization_code grant remains a follow-up.)
6. **Big subsystems** — delivered one opt-in subsystem at a time. **LDAP /
   Active Directory (done)** — the feature-gated `ldap` module. **ACL /
   domain-object security (done)** — `spring-security-acl` parity, pure Rust.
   **SAML2 SSO (done)** — the feature-gated `saml2` module: SP registration,
   SP-initiated `AuthnRequest`, and signed-response verification with replay
   protection (single-logout, signed `AuthnRequest`s, and encrypted assertions
   remain follow-ups).
