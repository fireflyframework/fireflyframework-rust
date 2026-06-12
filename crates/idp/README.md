# `firefly-idp`

> **Tier:** Adapter · **Status:** Full (port + types) · **Java original:** `firefly-idp` · **Go module:** `idp`

## Overview

`firefly-idp` is the **identity-provider port** every concrete IdP adapter
satisfies. It defines:

* `Adapter` — the async port (`login`, `refresh`, `validate`, plus
  user CRUD).
* `User` — the IdP-agnostic principal view.
* `Token` — the OIDC-shaped token envelope.
* `Error::InvalidCredentials`, `Error::UserNotFound` — the canonical
  error sentinels (`Error::Provider` carries adapter-specific failures).

Concrete implementations live in dedicated crates:

| Adapter                   | Backing tech                              | Status                      |
|---------------------------|-------------------------------------------|-----------------------------|
| `firefly-idp-internal-db` | bcrypt + HS256 JWT, in-memory user store  | **Full**                    |
| `firefly-idp-keycloak`    | Keycloak OIDC + admin REST                | Stub (sentinel-error guard) |
| `firefly-idp-azure-ad`    | MSAL + Microsoft Graph                    | Stub (sentinel-error guard) |
| `firefly-idp-aws-cognito` | AWS Cognito SDK                           | Stub (sentinel-error guard) |

## Public surface

```rust
pub struct User {
    pub id: String,
    pub username: String,
    pub email: String,                                    // omitted when empty
    pub roles: Vec<String>,                               // omitted when empty
    pub attributes: HashMap<String, serde_json::Value>,   // omitted when empty
    pub enabled: bool,
    pub created_at: DateTime<Utc>,                        // "createdAt"
}

pub struct Token {
    pub access_token: String,
    pub token_type: String,        // "Bearer"
    pub expires_in: i64,
    pub refresh_token: String,     // omitted when empty
    pub id_token: String,          // omitted when empty
    pub scope: String,             // omitted when empty
    pub issued_at: DateTime<Utc>,
}

#[async_trait]
pub trait Adapter: Send + Sync {
    async fn login(&self, username: &str, password: &str) -> Result<Token>;
    async fn refresh(&self, refresh_token: &str) -> Result<Token>;
    async fn validate(&self, access_token: &str) -> Result<User>;
    async fn get_user(&self, id: &str) -> Result<User>;
    async fn create_user(&self, user: User, password: &str) -> Result<User>;
    async fn update_user(&self, user: User) -> Result<User>;
    async fn delete_user(&self, id: &str) -> Result<()>;
    fn name(&self) -> &str;
}

pub enum Error { InvalidCredentials, UserNotFound, Provider(String) }
```

`User` and `Token` serialize with exactly the same JSON field names and
empty-field omission rules as the Go port (`encoding/json` `omitempty`
semantics), and the sentinel error messages are bytes-equal across
runtimes (`firefly/idp: invalid credentials`, `firefly/idp: user not
found`) — SDKs can transparently swap providers *and* ports.

## Quick start

```rust
use firefly_idp::{Adapter, Error, Token};

/// Authenticate against any provider behind the port.
async fn sign_in(idp: &dyn Adapter, user: &str, pass: &str) -> Result<Token, String> {
    match idp.login(user, pass).await {
        Ok(token) => Ok(token),
        Err(Error::InvalidCredentials) => Err("wrong username or password".into()),
        Err(e) => Err(e.to_string()),
    }
}
```

## Wiring with `security`

The port slots straight into the bearer-token middleware: validate the
access token through the adapter and project the resulting `User` onto
the security `Authentication`.

```rust
use std::sync::Arc;
use firefly_idp::Adapter;

async fn verify(idp: Arc<dyn Adapter>, token: &str) -> Result<(String, String, Vec<String>), firefly_idp::Error> {
    let u = idp.validate(token).await?;
    Ok((u.id, u.username, u.roles)) // principal, username, roles
}
```

## Testing

```bash
cargo test -p firefly-idp
```

Sentinel-error guards ensure the canonical error variants exist, have
non-empty messages, and render bytes-equal to the Go port; wire-shape
tests pin the `User`/`Token` JSON encodings. The substantive end-to-end
tests live in `firefly-idp-internal-db`.

## pyfly parity

The port is widened to the full pyfly `IdpAdapter` surface. Every method
below is a **default trait method** returning `Error::NotSupported("<op>")`,
so adapters that predate this surface (the Keycloak / Azure AD / Cognito
vendor crates, and any third-party adapter) keep compiling unchanged — they
only override what they support. This mirrors pyfly's vendor adapters raising
`NotImplementedError` for provider-side operations (e.g. MFA).

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    // ... Go-parity required methods (login/refresh/validate/CRUD/name) ...

    // Extended surface (default body → Error::NotSupported):
    async fn logout(&self, access_token: &str) -> Result<bool>;
    async fn introspect(&self, access_token: &str) -> Result<SessionIntrospection>;
    async fn find_by_username(&self, username: &str) -> Result<User>;
    async fn list_users(&self, limit: usize) -> Result<Vec<User>>;
    async fn change_password(&self, user_id: &str, old: &str, new: &str) -> Result<bool>;
    async fn reset_password(&self, user_id: &str) -> Result<String>;
    async fn register_user(&self, user: User, password: &str) -> Result<User>;
    async fn get_user_info(&self, access_token: &str) -> Result<User>;
    async fn mfa_challenge(&self, user_id: &str) -> Result<MfaChallenge>;
    async fn mfa_verify(&self, challenge_id: &str, code: &str) -> Result<Token>;
    async fn get_roles(&self, user_id: &str) -> Result<Vec<Role>>;
    async fn assign_role(&self, user_id: &str, role: &str) -> Result<bool>;
    async fn revoke_role(&self, user_id: &str, role: &str) -> Result<bool>;
    async fn list_roles(&self) -> Result<Vec<Role>>;
}
```

New DTOs (JSON field names match pyfly's `IdpRole` / `MfaChallenge` /
`SessionIntrospection` dataclasses, empty optionals omitted):

```rust
pub struct Role { pub name: String, pub description: String, pub scopes: Vec<String> }
pub struct MfaChallenge { pub challenge_id: String, pub user_id: String, pub method: String }
pub struct SessionIntrospection {
    pub active: bool, pub user_id: String, pub username: String, pub scopes: Vec<String>,
}
```

New error variants:

* `Error::MfaRequired(MfaChallenge)` — the Rust analogue of pyfly's
  `AuthResult.mfa_required=True` login outcome. An MFA-enrolled `login`
  returns `Err(MfaRequired(challenge))` instead of a token; the caller
  completes the challenge with `mfa_verify`.
* `Error::NotSupported(String)` — names the unsupported operation, returned
  by the default method bodies.

pyfly returns booleans / `Optional` for several methods; the Rust port keeps
those semantics where they convey real information (`logout`/`change_password`/
`assign_role`/`revoke_role` return `Result<bool>`) and uses
`Error::UserNotFound` where pyfly returns `None`/raises for a missing entity
(`find_by_username`, `get_user_info`, `reset_password`).
