# `firefly-idp-keycloak`

> **Tier:** Adapter · **Status:** Stable · **Backing tech:** Keycloak — direct OIDC + Keycloak admin REST API over `reqwest`

## Overview

`firefly-idp-keycloak` is a real `firefly_idp::Adapter` for Keycloak. It talks
to a Keycloak server's REST API over `reqwest` — no Keycloak SDK is pulled in.

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
  an expiry margin (`max(expires_in - 10, 1)` seconds) against a monotonic
  deadline, so every later admin call reuses a live token.
* **User CRUD** against `/admin/realms/{realm}/users` (`create_user` parses the
  `Location` header tail for the new id; `get_user`, `find_by_username`,
  `update_user`, `delete_user`, `list_users`).
* **OIDC flows** against `/realms/{realm}/protocol/openid-connect/*` —
  password-grant `login`, `refresh`, token `introspect`, `logout`, and
  `get_user_info` (userinfo). `validate` resolves a token via userinfo.
* **Password reset/change** via the admin `reset-password` endpoint.
* **Realm role-mappings** — `assign_role`, `revoke_role`, `list_roles`,
  `get_roles`.
* **TOTP MFA (real admin REST)** — `mfa_challenge` registers the
  `CONFIGURE_TOTP` required action via `PUT /admin/realms/{realm}/users/{id}`
  so Keycloak prompts TOTP enrollment at next sign-in. The admin OTP-credential
  endpoints Keycloak does expose are wrapped as `list_otp_credentials` (`GET
  .../users/{id}/credentials`, filtered to `type == "otp"`) and
  `remove_otp_credential` (`DELETE .../users/{id}/credentials/{credId}`).

`mfa_verify` is a **documented provider capability boundary**: Keycloak has no
admin REST endpoint to verify a TOTP code out-of-band (verification happens
server-side on the OTP form during the interactive browser login), so it returns
the precise typed `Error::UnsupportedByProvider { provider: "keycloak",
operation: "mfa_verify", reason: ... }` rather than a stub.

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

## Login variants

The port's `login` returns only the stateless `Token`. When you also need the
authenticated user record, `login_full` performs the follow-up lookup and
returns an `AuthResult` carrying both the user and the token.

## Testing

```bash
cargo test -p firefly-idp-keycloak
```

Behavior tests (`tests/keycloak_behavior.rs`) drive the real `reqwest` path
against an in-process `axum` mock server (port 0, no network, no Docker),
asserting both the outbound request shape (URL, verb, form/JSON body, auth
headers) and the parsed domain object.
