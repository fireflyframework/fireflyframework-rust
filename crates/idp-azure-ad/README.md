# `firefly-idp-azure-ad`

> **Tier:** Adapter · **Status:** Full · **Backing tech:** Microsoft Graph `v1.0` + `login.microsoftonline.com` ROPC over `reqwest`

## Overview

`firefly-idp-azure-ad` is a real `firefly_idp::Adapter` for Azure AD / Microsoft
Entra ID. It talks to the Microsoft Graph `v1.0` API and the
`login.microsoftonline.com` token endpoint over `reqwest` — no MSAL or Azure SDK
is pulled in — and is a behavior-for-behavior port of pyfly's
`AzureAdIdpAdapter`.

```rust
use firefly_idp::Adapter as _;
use firefly_idp_azure_ad::{Adapter, Config};

let idp = Adapter::new(Config {
    tenant: "contoso-tenant-id".into(),
    client_id: "app-client-id".into(),
    client_secret: "app-client-secret".into(),
    ..Config::default()
});
let token = idp.login("alice@contoso.com", "pw").await?;
```

## What it does

* **App-token caching** — the `client_credentials` Graph app token is fetched
  once and cached (mirroring pyfly).
* **ROPC login** — the resource-owner password-credentials grant against
  `https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token`, then a user
  lookup; a non-200 token response maps to `Error::InvalidCredentials`.
* **User CRUD** against `/users` (`create_user` POSTs the full profile +
  `passwordProfile` and captures the returned id; `find_by_username` delegates
  to `get_user` since Azure resolves the UPN as the id).
* **`/me` introspection / userinfo** with a delegated access token.
* **`passwordProfile` patch** for `change_password` / `reset_password`.
* **Groups-as-roles** — `assign_role` / `revoke_role` via
  `/groups/{id}/members/$ref`, `list_roles` via `/groups`, `get_roles` via
  `/users/{id}/memberOf`.

`mfa_challenge` / `mfa_verify` return the `ERR_NOT_IMPLEMENTED` sentinel because
Azure AD manages MFA natively via Conditional Access policies.

## Configuration

```rust
pub struct Config {
    pub base_url: String,       // login authority host override (default public host)
    pub graph_base_url: String, // Graph host override (default public host)
    pub realm: String,          // shared vendor-config field (unused)
    pub client_id: String,      // app (client) id
    pub client_secret: String,  // app secret
    pub tenant: String,         // directory (tenant) id
    pub scope: String,          // token scope (default graph .default)
    pub user_pool_id: String,   // shared vendor-config field (unused)
    pub region: String,         // shared vendor-config field (unused)
}
```

Empty `base_url` / `graph_base_url` / `scope` fall back to the public Microsoft
hosts and the `https://graph.microsoft.com/.default` scope; the host overrides
exist so the adapter can be exercised against an in-process mock.

## pyfly parity

| pyfly `AzureAdIdpAdapter` | Rust |
| --- | --- |
| `create_user` / `get_user` / `find_by_username` / `update_user` / `delete_user` / `list_users` | Graph `/users` CRUD |
| `login` → `AuthResult` | `login` → `Token` (port contract); `login_full` → `AuthResult` |
| `logout` (always true) / `refresh` / `introspect` (`/me`) | same |
| `change_password` / `reset_password` (`passwordProfile`) | same |
| `assign_role` / `revoke_role` / `list_roles` / `get_roles` | groups-as-roles |
| `get_user_info` (`/me`) / `register_user` | same |
| `mfa_challenge` / `mfa_verify` | sentinel (`ERR_NOT_IMPLEMENTED`) |

## Testing

```bash
cargo test -p firefly-idp-azure-ad
```

Behavior tests (`tests/azure_ad_behavior.rs`) drive the real `reqwest` path
against an in-process `axum` mock server (port 0, no network), asserting both
the outbound request shape and the parsed domain object — the Rust analog of
pyfly's `tests/idp/test_azure_ad_behavior.py`.
