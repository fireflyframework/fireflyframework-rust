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
| Method security (`#[pre_authorize]` / `#[post_authorize]`) | :status-supported: | Works uniformly across **bearer *and* session/OAuth2-login** auth |
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
| OAuth2 client (`AuthorizedClientManager`) / Authorization Server | :status-planned: | Login side present; outbound client + a mounted authorization server on the roadmap |
| ACL / domain-object security · SAML2 · LDAP/AD | :status-planned: | Roadmap (opt-in crates) |

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
4. **Method-security depth** — SpEL-style argument/principal binding,
   `@PreFilter`/`@PostFilter`, `PermissionEvaluator`.
5. **OAuth2 ecosystem** — opaque-token introspection, the outbound
   authorized-client manager, RP-initiated logout, a mounted authorization
   server.
6. **Big subsystems** — ACL / domain-object security, LDAP / Active Directory,
   SAML2.
