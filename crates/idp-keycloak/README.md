# `firefly-idp-keycloak`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** Keycloak — direct OIDC + Keycloak admin REST API

## Overview

`firefly-idp-keycloak` is the placeholder `firefly_idp::Adapter` adapter for
Keycloak — direct OIDC + Keycloak admin REST API. The crate and types are
declared, the port assertion compiles, and sentinel-error smoke tests guard
the wire shape — but the SaaS / cloud SDK integration is **not yet wired**.
Every method returns the not-implemented sentinel, bytes-equal to the Go
module's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpkeycloak: not yet implemented";
```

The sentinel travels as `firefly_idp::Error::Provider`, so callers match it
the same way they match any adapter-specific failure:

```rust
use firefly_idp::Adapter as _;
use firefly_idp_keycloak::{not_implemented, Adapter, Config};

let adapter = Adapter::new(Config::default());
assert_eq!(adapter.name(), "keycloak-stub");
assert_eq!(adapter.login("u", "p").await.unwrap_err(), not_implemented());
```

## Why ship a stub?

* The framework's tier diagram stays correct (no missing crate).
* The port boundary stays locked — when the real implementation lands,
  no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Configuration

```rust
pub struct Config {
    pub base_url: String,      // Keycloak server base URL
    pub realm: String,         // realm to authenticate against
    pub client_id: String,     // OIDC client id
    pub client_secret: String, // OIDC client secret
    pub tenant: String,        // shared vendor-stub field (unused here)
    pub user_pool_id: String,  // shared vendor-stub field (unused here)
    pub region: String,        // shared vendor-stub field (unused here)
}
```

Fields cover every wiring variable the production adapter needs; the shape is
field-for-field identical to the Go `idpkeycloak.Config` struct.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the production adapter. |
| `Adapter` | Placeholder `firefly_idp::Adapter`; `Adapter::new(cfg)` constructs it, `name()` returns `"keycloak-stub"`. |
| `ERR_NOT_IMPLEMENTED` | The sentinel message, bytes-equal to Go's `ErrNotImplemented`. |
| `not_implemented()` | The sentinel as `firefly_idp::Error::Provider`, for direct comparison. |

## Roadmap

The real implementation is scheduled for a later milestone — the Go port
tracks it for **v26.06.x** in `docs/AUDIT.md` § Roadmap.

## Testing

```bash
cargo test -p firefly-idp-keycloak
```

Smoke tests assert (a) port satisfaction behind `Arc<dyn firefly_idp::Adapter>`
and (b) every method returns the not-implemented sentinel. Once the production
adapter ships, these tests are deleted in favour of integration tests against
a real provider container / mock server.
