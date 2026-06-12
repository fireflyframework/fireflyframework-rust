# `firefly-ecm-storage-aws`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** AWS S3 (object storage)

## Overview

`firefly-ecm-storage-aws` is the placeholder `ContentStore` adapter for AWS S3
(object storage). The crate and types are declared, the port assertion
compiles, and sentinel-error smoke tests guard the wire shape — but the SaaS /
cloud SDK integration is **not yet wired**. Every method returns the
not-yet-implemented sentinel, carried through `firefly_ecm::EcmError::Provider`
so its rendered message is bytes-equal to the Go port's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmstorageaws: not yet implemented";
```

## Why ship a stub?

* The framework's tier diagram stays correct (no missing crate).
* The port boundary stays locked — when the real implementation lands
  in v26.06, no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Quick start

```rust
use firefly_ecm::ContentStore;
use firefly_ecm_storage_aws::{Config, Store, ERR_NOT_IMPLEMENTED};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let store = Store::new(Config {
        bucket: "firefly-docs".into(),
        region: "eu-west-1".into(),
        ..Default::default()
    });

    assert_eq!(store.name(), "ecmstorageaws-stub");

    // Every method returns the sentinel until the cloud SDK is wired.
    let err = store.delete("k").await.unwrap_err();
    assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
}
```

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs:
    // bucket, region, access_key, secret_key, endpoint, plus the
    // Azure-flavoured fields of the shared cloud-storage surface.
    // See `src/lib.rs` for the full set.
}
```

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the production adapter (bucket, region, credentials, endpoint, …). |
| `Store` | Placeholder `firefly_ecm::ContentStore`; `Store::new(Config)` constructs it, `Store::config()` exposes the retained wiring. |
| `ERR_NOT_IMPLEMENTED` | The Go-parity sentinel message. |
| `err_not_implemented()` | Builds the sentinel as `EcmError::Provider`. |
| `VERSION` | Framework version stamp. |

## Roadmap

The real implementation is scheduled for **v26.06.x** — see the Go port's
`docs/AUDIT.md` § Roadmap for sequencing.

## Testing

```bash
cargo test -p firefly-ecm-storage-aws
```

Smoke tests assert (a) port satisfaction (`Store: ContentStore`, object-safe
behind `Box`/`Arc`) and (b) every method returns the sentinel. Once the
production adapter ships, these tests are deleted in favour of integration
tests against a real provider container / mock server.
