# `firefly-ecm-storage-azure`

> **Tier:** Adapter · **Status:** Production · **Backing tech:** Azure Blob Storage

## Overview

`firefly-ecm-storage-azure` is the Azure Blob Storage `ContentStore` adapter.
`BlobStore` speaks the Azure Blob REST API directly over `reqwest`, authorizing
every request with a **self-contained Shared Key signer** (`hmac`/`sha2`/`base64`
— **no Azure SDK** is linked). Every operation is a real Blob REST call; there
are no stubbed methods.

It bridges `firefly_ecm::ContentReader` both ways and honours `Config.endpoint`,
so it works against Azurite or the in-process mock server used in tests.

## Operations

Each method maps to one real Blob REST API call:

| Method | Blob REST API | Request |
| --- | --- | --- |
| `ContentStore::put` | [`Put Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/put-blob) | `PUT /{container}/{blob}` block blob (drains the reader; returns bytes written) |
| `ContentStore::get` | [`Get Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/get-blob) | `GET /{container}/{blob}` (body as a reader; `404` → `NotFound`) |
| `ContentStore::delete` | [`Delete Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/delete-blob) | `DELETE /{container}/{blob}` (missing blob is not an error) |
| `BlobStore::list` | [`List Blobs`](https://learn.microsoft.com/en-us/rest/api/storageservices/list-blobs) | `GET /{container}?restype=container&comp=list&prefix=…` (parses `<Name>` from the XML) |
| `BlobStore::copy` | [`Copy Blob`](https://learn.microsoft.com/en-us/rest/api/storageservices/copy-blob) | `PUT /{container}/{dst}` with a signed `x-ms-copy-source` header |
| `BlobStore::properties` | [`Get Blob Properties`](https://learn.microsoft.com/en-us/rest/api/storageservices/get-blob-properties) | `HEAD /{container}/{blob}` (returns `BlobProperties`: size, MIME, ETag) |

`list`, `copy`, and `properties` are real provider capabilities exposed as
inherent methods on `BlobStore` on top of the four-method `ContentStore`
contract. For container-scoped requests the query parameters fold into the
Shared Key canonical resource as sorted `\nname:value` lines, so the signature
covers them.

## Verification and design notes

The adapter links no cloud SDK and exercises the **real** reqwest + Shared Key
code path against an in-process axum mock (`tests/blob_mock_test.rs`). The mock
asserts the HTTP method, container/blob path, body, and `x-ms-*` headers, and —
crucially — **recomputes the Shared Key signature server-side** to prove the
adapter signed exactly the request it sent (including the query-bearing List
Blobs canonical resource and the Copy Blob `x-ms-copy-source` header).

`BlobStore` is keyed by the opaque key the `ContentStore` port already exposes,
so a version-aware `<doc-id>/v<n>` key scheme maps straight onto blob names.
`BlobStore::name()` returns `azure-blob`.

### The Shared Key signer (`sharedkey` module)

The public `sharedkey` module implements the Azure Storage Shared Key
authorization scheme (the 13-line Blob string-to-sign, the `x-ms-*`
canonical-header block, and the `/<account>/<container>/<blob>` canonical
resource), signing with the base64-decoded account key. It is KAT-tested against
an independently computed HMAC-SHA256 reference signature using the public
Azurite development account key.

## Usage

```rust
use firefly_ecm::{bytes_reader, ContentStore};
use firefly_ecm_storage_azure::{BlobStore, Config};

#[tokio::main]
async fn main() -> Result<(), firefly_ecm::EcmError> {
    let store = BlobStore::new(Config {
        account: "fireflyacct".into(),
        key: "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==".into(),
        container: "documents".into(),
        // Optional: point at Azurite for local development.
        // endpoint: "http://127.0.0.1:10000/devstoreaccount1".into(),
        ..Default::default()
    })?;

    let n = store.put("doc-1/v1", bytes_reader(b"%PDF-1.7".to_vec())).await?;
    assert_eq!(n, 8);

    let names = store.list("doc-1/").await?;
    let props = store.properties("doc-1/v1").await?;
    let _ = (names, props);
    Ok(())
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

## Public surface

| Item | Description |
| --- | --- |
| `BlobStore` | The real `firefly_ecm::ContentStore` adapter over reqwest + Shared Key. `new(Config)` validates the wiring; `with_client` shares a `reqwest::Client`; `config()` exposes the wiring; `list` / `copy` / `properties` add the container-level operations. |
| `BlobProperties` | The `Get Blob Properties` result (`content_length`, `content_type`, `etag`). |
| `Config` | Wiring for both cloud adapters (`bucket`, `region`, `access_key`, `secret_key`, `account`, `key`, `container`, `endpoint`). |
| `sharedkey` | Self-contained Azure Shared Key signer (`sign`, `string_to_sign`, `Header`, `Request`). |
| `VERSION` | Framework version stamp. |

## Testing

```bash
cargo test -p firefly-ecm-storage-azure
```

* `sharedkey` unit tests verify the canonical headers, the string-to-sign
  shape, and a KAT signature against an independent HMAC-SHA256 reference.
* `tests/blob_mock_test.rs` runs `BlobStore` end-to-end against an in-process
  axum mock (port 0) that recomputes and verifies the Shared Key signature for
  put/get/delete **and** for list/copy/properties — no real Azure, no Docker.

No test talks to a real Azure endpoint; `cargo test` passes on a bare machine.
