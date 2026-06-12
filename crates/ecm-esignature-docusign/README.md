# `firefly-ecm-esignature-docusign`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** DocuSign — JWT-Bearer + REST v2.1

## Overview

`firefly-ecm-esignature-docusign` is the placeholder
[`firefly_ecm::ESignatureProvider`] adapter for DocuSign — JWT-Bearer + REST
v2.1. The crate and types are declared, the port assertion compiles, and
sentinel-error smoke tests guard the wire shape — but the SaaS / cloud SDK
integration is **not yet wired**. Every method returns the
`ERR_NOT_IMPLEMENTED` sentinel, byte-for-byte equal to the Go port's
`ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmesignaturedocusign: not yet implemented";
```

The sentinel is carried as `firefly_ecm::EcmError::Provider`, whose `Display`
output renders the message verbatim, so the error string observed by callers
is identical across the Go, Java, .NET, Python, and Rust ports.

## Why ship a stub?

* The framework's tier diagram stays correct (no missing module).
* The port boundary stays locked — when the real implementation lands,
  no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Quick start

```rust
use firefly_ecm::{ESignatureProvider, SignatureRequest};
use firefly_ecm_esignature_docusign::{is_not_implemented, Config, Provider};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let provider = Provider::new(Config::default());
    assert_eq!(provider.name(), "ecmesignaturedocusign-stub");

    let err = provider.create(SignatureRequest::default()).await.unwrap_err();
    assert!(is_not_implemented(&err));
    assert_eq!(
        err.to_string(),
        "firefly/ecmesignaturedocusign: not yet implemented",
    );
}
```

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs:
    // OAuth2 / JWT-grant wiring for DocuSign JWT-Bearer + REST v2.1.
    pub base_url: String,        // e.g. https://demo.docusign.net/restapi
    pub client_id: String,       // OAuth2 client identifier
    pub client_secret: String,   // OAuth2 client secret
    pub integration_key: String, // DocuSign integration key (JWT grant)
    pub user_guid: String,       // GUID of the impersonated user
}
```

The stub stores the configuration untouched (readable via
`Provider::config()`), so consuming code can wire its settings today and swap
in the real adapter without changes.

## Public surface

| Item | Description |
|---|---|
| `Config` | OAuth2 / JWT-grant wiring for the production adapter |
| `Provider` | Placeholder `ESignatureProvider`; `Provider::new(cfg)` |
| `ERR_NOT_IMPLEMENTED` | Sentinel message, bytes-equal to Go's `ErrNotImplemented` |
| `not_implemented()` | Builds the sentinel as `EcmError::Provider` |
| `is_not_implemented(&EcmError)` | Analog of Go's `errors.Is(err, ErrNotImplemented)` |
| `VERSION` | Framework version stamp |

## Roadmap

The real implementation is scheduled for a future release — see the Go
repository's `docs/AUDIT.md` § Roadmap for sequencing.

## Testing

```bash
cargo test -p firefly-ecm-esignature-docusign
```

Smoke tests assert (a) port satisfaction (the adapter coerces to
`Box<dyn ESignatureProvider>` / `Arc<dyn ESignatureProvider>`) and (b) every
method returns the `ERR_NOT_IMPLEMENTED` sentinel. Once the production adapter
ships, these tests are deleted in favour of integration tests against a real
provider container / mock server.
