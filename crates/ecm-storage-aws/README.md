# `firefly-ecm-storage-aws`

> **Tier:** Adapter · **Status:** Production · **Backing tech:** AWS S3 (object storage)

## Overview

`firefly-ecm-storage-aws` is the AWS S3 `ContentStore` adapter. `S3Store` speaks
the S3 REST API directly over `reqwest`, signing every request with a
**self-contained AWS Signature Version 4 implementation** (`hmac`/`sha2`/`hex` —
**no AWS SDK** is linked). Every operation is a real S3 REST call; there are no
stubbed methods.

It bridges `firefly_ecm::ContentReader` both ways and honours `Config.endpoint`,
so it works against LocalStack, MinIO, or the in-process mock server used in
tests.

## Operations

Each method maps to one real S3 REST API call:

| Method | S3 REST API | Request |
| --- | --- | --- |
| `ContentStore::put` | [`PutObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html) | `PUT /{bucket}/{key}` (drains the reader; returns bytes written) |
| `ContentStore::get` | [`GetObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html) | `GET /{bucket}/{key}` (body as a reader; `404` → `NotFound`) |
| `ContentStore::delete` | [`DeleteObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_DeleteObject.html) | `DELETE /{bucket}/{key}` (missing key is not an error) |
| `S3Store::list` | [`ListObjectsV2`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html) | `GET /{bucket}?list-type=2&prefix=…` (parses `<Key>` from the XML) |
| `S3Store::copy` | [`CopyObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_CopyObject.html) | `PUT /{bucket}/{dst}` with a signed `x-amz-copy-source` header |
| `S3Store::head` | [`HeadObject`](https://docs.aws.amazon.com/AmazonS3/latest/API/API_HeadObject.html) | `HEAD /{bucket}/{key}` (returns `ObjectMetadata`: size, MIME, ETag) |
| `S3Store::presign_get` | [SigV4 query-string auth](https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html) | locally-computed presigned `GET` URL (no network call) |

`list`, `copy`, `head`, and `presign_get` are real provider capabilities exposed
as inherent methods on `S3Store` on top of the four-method `ContentStore`
contract.

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

The public `sigv4` module is a from-scratch implementation of the four AWS SigV4
steps (canonical request → string-to-sign → signing key → signature), validated
against the **official AWS SigV4 test-suite Known Answer Test vectors**
(`get-vanilla`, `get-vanilla-query`, `post-header-key-sort`, plus the derived
signing key) so its output is byte-for-byte AWS's reference signer. It also
exposes `presign_signature`, the query-string-authentication variant used by
`S3Store::presign_get`.

## Quick start

```rust
use firefly_ecm::{bytes_reader, ContentStore};
use firefly_ecm_storage_aws::{Config, S3Store};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), firefly_ecm::EcmError> {
    let store = S3Store::new(Config {
        bucket: "firefly-docs".into(),
        region: "eu-west-1".into(),
        access_key: "AKIA…".into(),
        secret_key: "s3cr3t".into(),
        // Optional: point at LocalStack / MinIO for local development.
        // endpoint: "http://localhost:4566".into(),
        ..Default::default()
    })?;

    let n = store.put("doc-1/v1", bytes_reader(b"%PDF-1.7".to_vec())).await?;
    assert_eq!(n, 8);

    let keys = store.list("doc-1/", 100).await?;
    let meta = store.head("doc-1/v1").await?;
    let url = store.presign_get("doc-1/v1", 900)?;
    let _ = (keys, meta, url);
    Ok(())
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
| `S3Store` | The real `firefly_ecm::ContentStore` adapter over reqwest + SigV4. `new(Config)` validates the wiring; `with_client` shares a `reqwest::Client`; `config()` exposes the wiring; `list` / `copy` / `head` / `presign_get` add the bucket-level operations. |
| `ObjectMetadata` | The `HeadObject` result (`content_length`, `content_type`, `etag`). |
| `Config` | Typed wiring for both cloud adapters (bucket, region, credentials, endpoint, …). |
| `sigv4` | Self-contained AWS Signature Version 4 signer (`sign`, `presign_signature`, `canonical_request`, `sha256_hex`, `EMPTY_PAYLOAD_SHA256`, `UNSIGNED_PAYLOAD`). |
| `VERSION` | Framework version stamp. |

## Testing

```bash
cargo test -p firefly-ecm-storage-aws
```

* `sigv4` unit tests verify the signer against the official AWS SigV4
  test-suite KAT vectors.
* `tests/s3_mock_test.rs` runs `S3Store` end-to-end against an in-process axum
  mock S3 server (port 0), asserting the canonical request and SigV4 auth
  headers for put/get/delete **and** for list/copy/head/presign — no real AWS,
  no Docker.

No test talks to a real AWS endpoint; `cargo test` passes on a bare machine.
