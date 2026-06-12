# `firefly-ecm-storage-azure`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** Azure Blob Storage

## Overview

`firefly-ecm-storage-azure` is the placeholder `ContentStore` adapter for
Azure Blob Storage. The crate and types are declared, the port assertion
compiles, and sentinel-error smoke tests guard the wire shape — but the
SaaS / cloud SDK integration is **not yet wired**. Every method returns
the not-yet-implemented sentinel, carried as a
`firefly_ecm::EcmError::Provider` whose message is bytes-equal to the Go
port's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmstorageazure: not yet implemented";
```

Faithful port of the Go module `fireflyframework-go/ecmstorageazure` (itself a
direct port of the Java `firefly-ecm-storage-azure` module and the .NET
`FireflyFramework.Ecm.Storage.*` project).

## Why ship a stub?

* The framework's tier diagram stays correct (no missing module).
* The port boundary stays locked — when the real implementation lands
  in v26.06, no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Wiring needed by the production adapter (`bucket`, `region`, `access_key`, `secret_key`, `account`, `key`, `container`, `endpoint`). |
| `Store` | The placeholder `firefly_ecm::ContentStore` adapter; `Store::new(Config)` constructs it, `Store::config()` exposes the captured wiring. |
| `ERR_NOT_IMPLEMENTED` | The sentinel message returned by every method. |
| `err_not_implemented()` | Builds the sentinel as an `EcmError::Provider`. |
| `is_not_implemented(&EcmError)` | The analog of Go's `errors.Is(err, ErrNotImplemented)`. |
| `VERSION` | Framework version stamp. |

## Usage

```rust
use firefly_ecm::{bytes_reader, ContentStore};
use firefly_ecm_storage_azure::{is_not_implemented, Config, Store};

#[tokio::main]
async fn main() {
    let store = Store::new(Config {
        account: "fireflyacct".into(),
        container: "documents".into(),
        ..Default::default()
    });
    assert_eq!(store.name(), "ecmstorageazure-stub");

    let err = store.put("k", bytes_reader(b"x".to_vec())).await.unwrap_err();
    assert!(is_not_implemented(&err));
    assert_eq!(err.to_string(), "firefly/ecmstorageazure: not yet implemented");
}
```

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs.
    // The Azure-specific ones are `account`, `key`, and `container`;
    // see `src/lib.rs` for the full set.
}
```

## Roadmap

The real implementation is scheduled for **v26.06.x** — see the Go port's
`docs/AUDIT.md` § Roadmap for sequencing.

## Testing

```bash
cargo test -p firefly-ecm-storage-azure
```

Smoke tests assert (a) port satisfaction (`Store` coerces to
`Box<dyn ContentStore>` / `Arc<dyn ContentStore>`) and (b) every method
returns the not-yet-implemented sentinel, rendered bytes-equal to the Go
module. Once the production adapter ships, these tests are deleted in
favour of integration tests against a real provider container / mock
server.
