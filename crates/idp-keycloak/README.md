# `firefly-idp-keycloak`

> **Tier:** Adapter · **Status:** Full · **Backing tech:** Keycloak — direct OIDC + Keycloak admin REST API over `reqwest`

## Overview

`firefly-idp-keycloak` is a real `firefly_idp::Adapter` for Keycloak. It talks
to a Keycloak server's REST API over `reqwest` — no Keycloak SDK is pulled in —
and is a behavior-for-behavior port of pyfly's `KeycloakIdpAdapter`.

```rust
use firefly_idp::Adapter as _;
use firefly_idp_keycloak::{Adapter, Config};

let idp = Adapter::new(Config {
    base_url: "https://keycloak.example.com".into(),
    realm: "firefly".into(),
    client_id: "admin-cli".into(),
    client_secret: "s3cret".into(),
    ..Config::default()
});
let token = idp.login("alice", "pw").await?;
```

## What it does

* **Admin grant caching** — the `client_credentials` admin token is cached with
  an expiry margin (`max(expires_in - 10, 1)` seconds), mirroring pyfly's
  monotonic deadline, so every later admin call reuses a live token.
* **User CRUD** against `/admin/realms/{realm}/users` (`create_user` parses the
  `Location` header tail for the new id; `get_user`, `find_by_username`,
  `update_user`, `delete_user`, `list_users`).
* **OIDC flows** against `/realms/{realm}/protocol/openid-connect/*` —
  password-grant `login`, `refresh`, token `introspect`, `logout`, and
  `get_user_info` (userinfo). `validate` resolves a token via userinfo.
* **Password reset/change** via the admin `reset-password` endpoint.
* **Realm role-mappings** — `assign_role`, `revoke_role`, `list_roles`,
  `get_roles`.

`mfa_challenge` / `mfa_verify` return the `ERR_NOT_IMPLEMENTED` sentinel because
Keycloak performs MFA server-side during the browser auth flow — exactly as
pyfly raises `NotImplementedError`.

## Configuration

```rust
pub struct Config {
    pub base_url: String,      // Keycloak server base URL (trailing / trimmed)
    pub realm: String,         // realm to authenticate against
    pub client_id: String,     // OIDC client id
    pub client_secret: String, // OIDC client secret
    pub verify_ssl: bool,      // verify TLS certificates (default true)
    pub tenant: String,        // shared vendor-config field (unused here)
    pub user_pool_id: String,  // shared vendor-config field (unused here)
    pub region: String,        // shared vendor-config field (unused here)
}
```

## pyfly parity

| pyfly `KeycloakIdpAdapter` | Rust |
| --- | --- |
| `create_user` / `get_user` / `find_by_username` / `update_user` / `delete_user` / `list_users` | same; CRUD against the admin users API |
| `login` → `AuthResult` | `login` → `Token` (port contract); `login_full` → `AuthResult` (user + token) |
| `logout` / `refresh` / `introspect` | same |
| `change_password` / `reset_password` | same |
| `assign_role` / `revoke_role` / `list_roles` / `get_roles` | realm role-mappings |
| `get_user_info` / `register_user` | userinfo / self-registration |
| `mfa_challenge` / `mfa_verify` | sentinel (`ERR_NOT_IMPLEMENTED`) |

`login`'s `AuthResult.user` follow-up lookup is preserved in `login_full`; the
port's `login` returns only the stateless `Token`.

## Testing

```bash
cargo test -p firefly-idp-keycloak
```

Behavior tests (`tests/keycloak_behavior.rs`) drive the real `reqwest` path
against an in-process `axum` mock server (port 0, no network, no Docker),
asserting both the outbound request shape (URL, verb, form/JSON body, auth
headers) and the parsed domain object — the Rust analog of pyfly's
`tests/idp/test_keycloak_behavior.py`.
