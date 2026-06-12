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
