# `firefly-idp-azure-ad`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** Azure AD / Entra ID — MSAL + Microsoft Graph · **Go module:** `idpazuread`

## Overview

`firefly-idp-azure-ad` is the placeholder `firefly_idp::Adapter` for
Azure AD / Entra ID — MSAL + Microsoft Graph. The crate and types are
declared, the port implementation compiles, and sentinel-error smoke
tests guard the wire shape — but the SaaS / cloud SDK integration is
**not yet wired**. Every method returns the not-yet-implemented
sentinel, bytes-equal to the Go port's `idpazuread.ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpazuread: not yet implemented";

/// Builds the sentinel as a `firefly_idp::Error::Provider`.
pub fn not_implemented() -> firefly_idp::Error;
```

## Why ship a stub?

* The framework's tier diagram stays correct (no missing module).
* The port boundary stays locked — when the real implementation lands
  in v26.06, no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs.
    pub base_url: String,
    pub realm: String,
    pub client_id: String,
    pub client_secret: String,
    pub tenant: String,
    pub user_pool_id: String, // shared vendor-config field; unused here
    pub region: String,       // shared vendor-config field; unused here
}
```

## Quick start

```rust
use std::sync::Arc;
use firefly_idp_azure_ad::{Adapter, Config, ERR_NOT_IMPLEMENTED};

let idp: Arc<dyn firefly_idp::Adapter> = Arc::new(Adapter::new(Config::default()));
assert_eq!(idp.name(), "azuread-stub");

// Every port method returns the sentinel until the adapter ships:
// idp.login("u", "p").await == Err(Error::Provider(ERR_NOT_IMPLEMENTED.into()))
```

## Roadmap

The real implementation is scheduled for **v26.06.x** — see the Go
port's `docs/AUDIT.md` § Roadmap for sequencing.

## Testing

```bash
cargo test -p firefly-idp-azure-ad
```

Smoke tests assert (a) port satisfaction (`Arc<dyn firefly_idp::Adapter>`
from `Adapter::new(Config::default())`) and (b) every method returns the
`ERR_NOT_IMPLEMENTED` sentinel. Once the production adapter ships, these
tests are deleted in favour of integration tests against a real
provider container / mock server.
