# `firefly-ecm-storage-azure`

> **Tier:** Adapter · **Status:** Production (`BlobStore`) + back-compat stub (`Store`) · **Backing tech:** Azure Blob Storage

## Overview

`firefly-ecm-storage-azure` is the Azure Blob Storage `ContentStore` adapter. It
ships two flavours that share one `Config`:

* **`BlobStore`** — the real adapter (pyfly parity). It speaks the Azure Blob
  REST API directly over `reqwest`, authorizing every request with a
  **self-contained Shared Key signer** (`hmac`/`sha2`/`base64` — **no Azure
  SDK** is linked). It bridges `firefly_ecm::ContentReader` both ways: `put`
  drains the reader and `PUT`s a block blob, `get` returns the blob body as a
  reader. It honours `Config.endpoint`, so it works against Azurite or the
  in-process mock server used in tests.
* **`Store`** — the original Go-parity stub, retained for backward
  compatibility. Every method returns the not-yet-implemented sentinel, carried
  as a `firefly_ecm::EcmError::Provider` whose message is bytes-equal to the Go
  port's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmstorageazure: not yet implemented";
```

## pyfly parity

This crate is the Rust analog of pyfly's `pyfly.ecm.adapters.azure_blob`
(`AzureBlobStorageAdapter`), adapted to the Rust `ContentStore` port:

* pyfly wraps `azure-storage-blob` and injects a fake `BlobServiceClient` in
  tests; Rust avoids any cloud SDK and exercises the **real** reqwest + Shared
  Key code path against an in-process axum mock (`tests/blob_mock_test.rs`). The
  mock asserts the HTTP method, container/blob path, body, and `x-ms-*` headers,
  and — crucially — **recomputes the Shared Key signature server-side** to prove
  the adapter signed exactly the request it sent.
* The version-aware `<doc-id>/v<n>` key scheme pyfly uses maps straight onto
  blob names — `BlobStore` is keyed by the opaque key the `ContentStore` port
  already exposes.
* `BlobStore::name()` returns `azure-blob`, matching
  `AzureBlobStorageAdapter.name`.

### The Shared Key signer (`sharedkey` module)

The public `sharedkey` module implements the Azure Storage Shared Key
authorization scheme (the 13-line Blob string-to-sign, the `x-ms-*`
canonical-header block, and the `/<account>/<container>/<blob>` canonical
resource), signing with the base64-decoded account key. It is KAT-tested against
an independently computed HMAC-SHA256 reference signature using the public
Azurite development account key.

## Why keep the stub?

* The port boundary stays locked — existing consumers of `Store`,
  `ERR_NOT_IMPLEMENTED`, and `is_not_implemented` keep compiling unchanged.
* The Go-parity wire contract stays covered by the original smoke tests.

## Public surface

| Item | Description |
| --- | --- |
| `BlobStore` | The real `firefly_ecm::ContentStore` adapter over reqwest + Shared Key. `BlobStore::new(Config)` validates the wiring; `with_client` shares a `reqwest::Client`; `config()` exposes the wiring. |
| `Config` | Wiring for both adapters (`bucket`, `region`, `access_key`, `secret_key`, `account`, `key`, `container`, `endpoint`). |
| `sharedkey` | Self-contained Azure Shared Key signer (`sign`, `string_to_sign`, `Header`, `Request`). |
| `Store` | Back-compat placeholder `firefly_ecm::ContentStore`; `Store::new(Config)` / `Store::config()`. |
| `ERR_NOT_IMPLEMENTED` | The sentinel message returned by every stub method. |
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

## Testing

```bash
cargo test -p firefly-ecm-storage-azure
```

* `sharedkey` unit tests verify the canonical headers, the string-to-sign
  shape, and a KAT signature against an independent HMAC-SHA256 reference.
* `tests/blob_mock_test.rs` runs `BlobStore` end-to-end against an in-process
  axum mock (port 0) that recomputes and verifies the Shared Key signature — no
  real Azure, no Docker.
* The original smoke tests still guard the back-compat `Store` stub.

No test talks to a real Azure endpoint; `cargo test` passes on a bare machine.
