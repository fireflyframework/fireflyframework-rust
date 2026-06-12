# `firefly-idp-internal-db`

> **Tier:** Adapter · **Status:** Full · **Java original:** `firefly-idp-internal-db` · **Go module:** `idpinternaldb`

## Overview

`firefly-idp-internal-db` is a self-contained [`firefly_idp::Adapter`]
implementation backed by an in-process user store with **bcrypt-hashed
passwords** and **HMAC-SHA256-signed JWT** access / refresh tokens.
Suitable for:

* Development and integration tests.
* Small standalone services that don't need an external IDP.
* The default IDP wiring of the Orders sample.

For production deployments against Keycloak / Azure AD / AWS Cognito,
swap in the corresponding adapter — the calling code (e.g. the
`firefly-security` bearer-middleware verifier) doesn't change.

## Configuration

```rust,ignore
pub struct Config {
    pub jwt_secret: Vec<u8>,          // required — signs and verifies tokens
    pub token_ttl: std::time::Duration, // defaults to 1 h (zero is clamped)
    pub issuer: String,               // surfaced in the iss claim
}
impl Default for Config { /* 1 h TTL, issuer "firefly-internal-db" */ }
```

## Token shape

JWT header carries `alg: HS256`, `typ: JWT`. Claims (serialized in this
exact order — byte-identical to the Go port's sorted-map encoding):

```json
{
  "exp": 1700003600,
  "iat": 1700000000,
  "iss": "firefly-internal-db",
  "roles": ["USER", "ADMIN"],
  "sub": "<user-id>",
  "un":  "<username>"
}
```

`roles` is omitted when the user has none. Signatures are HMAC-SHA256
over `base64url(header) + "." + base64url(claims)` with unpadded
URL-safe base64 — tokens minted by the Go port verify here and vice
versa (verification is independent of header key order). Verification
matches Go: only the signature and — when present — `exp` are checked;
every other claim (`aud`, `iss`, `nbf`, …) is ignored.

The refresh token is currently the same value as the access token —
adequate for in-process testing; production deployments should use
the proper IDP modules with distinct refresh-token semantics.

## Public surface

```rust,ignore
pub struct Config { pub jwt_secret: Vec<u8>, pub token_ttl: Duration, pub issuer: String }
pub struct Adapter { /* private */ }
impl Adapter { pub fn new(cfg: Config) -> Self }

// Implements every firefly_idp::Adapter method:
async fn login(&self, username: &str, password: &str) -> Result<Token>;
async fn refresh(&self, refresh_token: &str) -> Result<Token>;
async fn validate(&self, access_token: &str) -> Result<User>;
async fn get_user(&self, id: &str) -> Result<User>;
async fn create_user(&self, user: User, password: &str) -> Result<User>;
async fn update_user(&self, user: User) -> Result<User>;
async fn delete_user(&self, id: &str) -> Result<()>;
fn name(&self) -> &str; // "internal-db"
```

`login` returns `Error::InvalidCredentials` on bad password / unknown
user. `validate` returns `Error::UserNotFound` if the token verifies
but the user has since been deleted. Verification failures surface as
`Error::Provider` with the Go port's exact messages
(`idp/internal-db: malformed jwt`, `idp/internal-db: bad signature`,
`idp/internal-db: token expired`); a duplicate id on `create_user`
renders as `idp/internal-db: id "<id>" already exists`.

## Quick start

```rust,ignore
use firefly_idp::{Adapter as _, User};
use firefly_idp_internal_db::{Adapter, Config};

let idp = Adapter::new(Config {
    jwt_secret: b"super-secret-please-rotate".to_vec(),
    token_ttl: std::time::Duration::from_secs(3600),
    issuer: "orders-service".into(),
});

idp.create_user(
    User {
        username: "alice".into(),
        email: "alice@example.com".into(),
        roles: vec!["USER".into()],
        enabled: true,
        ..User::default()
    },
    "Hunter-2-pass!",
)
.await?;
let token = idp.login("alice", "Hunter-2-pass!").await?;
let user = idp.validate(&token.access_token).await?;
```

## Testing

```bash
cargo test -p firefly-idp-internal-db
```

Covers create + login + validate + refresh + delete lifecycle, bad
credentials, deleted-user post-validation, a port assertion
(`Arc<dyn firefly_idp::Adapter>`), plus Rust-specific guards: TTL
clamping, duplicate-id rejection, exact Go error strings for
malformed / forged / expired tokens, byte-for-byte claim-shape pinning,
acceptance of Go-minted tokens, and concurrent logins through a shared
`Arc<dyn Adapter>`.

## pyfly parity

This adapter implements the full extended `firefly_idp::Adapter` surface
(ported from pyfly's `InternalDbIdpAdapter`):

* **Opaque-token registry** — every `login` / `refresh` / `mfa_verify`
  records the minted access and refresh tokens in an in-process registry
  (`token → user_id`) *in addition to* returning the stateless JWT. This is
  what makes server-side `logout` (revoke) and `introspect` (RFC 7662)
  possible without changing the Go-parity JWT wire shape. `introspect`
  reports an inactive session for a revoked token or a since-deleted user.
* **Role catalogue** — `assign_role` / `revoke_role` mutate the user's roles
  and register the role name in a catalogue; `create_roles`,
  `set_role_description`, `list_roles`, and `get_roles` (catalogue-enriched)
  round-trip role metadata. `register_user` forces `enabled=true` and strips
  the privileged `admin` role.
* **TOTP MFA (RFC 6238)** — hand-rolled over the workspace `hmac` + `sha2` +
  `base64` crates (no external TOTP dependency). `enable_mfa` enrolls a
  base32 secret; an MFA-enrolled `login` returns
  `Err(Error::MfaRequired(challenge))` (the analogue of pyfly's
  `mfa_required=True`); `mfa_verify` consumes the single-use challenge,
  checks the code, and mints tokens. `current_totp` is a test/automation
  helper that generates the live code.

> **TOTP hash divergence:** pyfly uses `pyotp`, which defaults to
> HMAC-**SHA1**. The Rust workspace ships only `sha2`, so this port uses
> HMAC-**SHA256** per the parity brief. The implementation is self-consistent
> (it both mints and verifies codes) so the behavioral MFA flow matches pyfly
> exactly; codes are *not* interchangeable with a SHA1 authenticator app. The
> `totp` module is cross-checked against the **RFC 6238 Appendix B** SHA-256
> known-answer vectors.

The pyfly test cases (`test_idp.py`, `test_idp_mfa_and_extensions.py`) are
ported as unit tests: create/login/introspect/logout, login failure,
change/reset password, role assign/revoke, the full MFA enable → challenge →
verify flow (incl. wrong-code and consumed-challenge rejection),
`get_user_info`, `register_user` defaults, and catalogue-enriched
`get_roles`.
