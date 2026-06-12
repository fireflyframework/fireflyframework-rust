# `firefly-ecm-storage-aws`

> **Tier:** Adapter · **Status:** Production (`S3Store`) + back-compat stub (`Store`) · **Backing tech:** AWS S3 (object storage)

## Overview

`firefly-ecm-storage-aws` is the AWS S3 `ContentStore` adapter. It ships two
flavours that share one `Config`:

* **`S3Store`** — the real adapter (pyfly parity). It speaks the S3 REST API
  directly over `reqwest`, signing every request with a **self-contained AWS
  Signature Version 4 implementation** (`hmac`/`sha2`/`hex` — **no AWS SDK** is
  linked). It bridges `firefly_ecm::ContentReader` both ways: `put` drains the
  reader and `PUT`s the bytes, `get` returns the object body as a reader. It
  honours `Config.endpoint`, so it works against LocalStack, MinIO, or the
  in-process mock server used in tests.
* **`Store`** — the original Go-parity stub, retained for backward
  compatibility. Every method returns the not-yet-implemented sentinel, carried
  through `firefly_ecm::EcmError::Provider` so its rendered message is
  bytes-equal to the Go port's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmstorageaws: not yet implemented";
```

## pyfly parity

This crate is the Rust analog of pyfly's `pyfly.ecm.adapters.aws_s3`
(`AwsS3StorageAdapter`), adapted to the Rust `ContentStore` port:

* pyfly wraps boto3 and injects a fake client in tests; Rust avoids any cloud
  SDK and instead exercises the **real** reqwest + SigV4 code path against an
  in-process axum mock server (`tests/s3_mock_test.rs`), asserting the HTTP
  method, the bucket-and-key path, the request body, and the SigV4
  `Authorization` / `x-amz-*` headers.
* The version-aware `<doc-id>/v<n>` key scheme pyfly uses for storage URIs maps
  straight onto S3 object keys — `S3Store` is keyed by the opaque object key the
  `ContentStore` port already exposes.
* `S3Store::name()` returns `aws-s3`, matching `AwsS3StorageAdapter.name`.

### The SigV4 signer (`sigv4` module)

The public `sigv4` module is a from-scratch, ~200-LOC implementation of the four
AWS SigV4 steps (canonical request → string-to-sign → signing key → signature),
validated against the **official AWS SigV4 test-suite Known Answer Test
vectors** (`get-vanilla`, `get-vanilla-query`, `post-header-key-sort`, plus the
derived signing key) so its output is byte-for-byte AWS's reference signer.

## Why keep the stub?

* The port boundary stays locked — existing consumers of `Store` and
  `ERR_NOT_IMPLEMENTED` keep compiling unchanged.
* The Go-parity wire contract stays covered by the original smoke tests.

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
| `S3Store` | The real `firefly_ecm::ContentStore` adapter over reqwest + SigV4. `S3Store::new(Config)` validates the wiring; `with_client` shares a `reqwest::Client`; `config()` exposes the wiring. |
| `Config` | Typed wiring for both adapters (bucket, region, credentials, endpoint, …). |
| `sigv4` | Self-contained AWS Signature Version 4 signer (`sign`, `canonical_request`, `sha256_hex`, `EMPTY_PAYLOAD_SHA256`, `UNSIGNED_PAYLOAD`). |
| `Store` | Back-compat placeholder `firefly_ecm::ContentStore` returning the sentinel; `Store::new(Config)` / `Store::config()`. |
| `ERR_NOT_IMPLEMENTED` | The Go-parity sentinel message. |
| `err_not_implemented()` | Builds the sentinel as `EcmError::Provider`. |
| `VERSION` | Framework version stamp. |

## Testing

```bash
cargo test -p firefly-ecm-storage-aws
```

* `sigv4` unit tests verify the signer against the official AWS SigV4
  test-suite KAT vectors.
* `tests/s3_mock_test.rs` runs `S3Store` end-to-end against an in-process axum
  mock S3 server (port 0), asserting the canonical request and SigV4 auth
  headers — no real AWS, no Docker.
* The original smoke tests still guard the back-compat `Store` stub.

No test talks to a real AWS endpoint; `cargo test` passes on a bare machine.
