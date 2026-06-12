//! Env-gated live integration test for [`firefly_ecm_storage_aws::S3Store`]
//! against a real S3-compatible endpoint (LocalStack).
//!
//! This is an **env-gated** integration test, not `#[ignore]`-gated. It reads
//! `FIREFLY_TEST_S3_ENDPOINT` (e.g. `http://localhost:4566`); when that
//! variable is **unset** it prints a one-line `skipping …` and returns, so
//! `cargo test` on a bare machine is green. When it is **set** the test creates
//! a fresh bucket, then exercises the full object round-trip through the store
//! (put → get → list → head → copy → delete) and finally deletes the bucket.
//!
//! Run against the docker-compose stack with:
//!
//! ```sh
//! export FIREFLY_TEST_S3_ENDPOINT="http://localhost:4566"
//! cargo test -p firefly-ecm-storage-aws --test s3_integration
//! ```
//!
//! Bucket and key names are derived from the test fn name, the process id, and
//! a process-wide atomic counter (never a random source), so concurrent runs
//! against one LocalStack never collide and every test cleans up after itself.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use firefly_ecm::{bytes_reader, ContentStore, EcmError};
use firefly_ecm_storage_aws::{sigv4, Config, S3Store};
use tokio::io::AsyncReadExt;

/// LocalStack accepts any credentials; these are the conventional dummies.
const ACCESS_KEY: &str = "test";
const SECRET_KEY: &str = "test";
const REGION: &str = "us-east-1";

/// Process-wide monotonic counter for unique bucket/key names.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the S3 endpoint from the standard env var. Returns `None` when unset
/// so callers can early-skip.
fn s3_endpoint() -> Option<String> {
    std::env::var("FIREFLY_TEST_S3_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
}

/// A unique, DNS-safe bucket name for this `slug` (lowercase, hyphenated).
fn unique_bucket(slug: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("fftest-s3-{slug}-{}-{n}", std::process::id())
}

/// Builds a path-style store targeting `bucket` on the integration endpoint.
fn store(endpoint: &str, bucket: &str) -> S3Store {
    S3Store::new(Config {
        bucket: bucket.into(),
        region: REGION.into(),
        access_key: ACCESS_KEY.into(),
        secret_key: SECRET_KEY.into(),
        endpoint: endpoint.into(),
        ..Default::default()
    })
    .unwrap()
}

/// Issues a SigV4-signed bucket-level request (`PUT` to create, `DELETE` to
/// remove) against the path-style endpoint, returning the HTTP status code.
///
/// The store's own put/get/delete cover object operations; bucket lifecycle is
/// not part of the `ContentStore` port, so the test signs these two calls
/// directly with the crate's public [`sigv4`] signer — the same code path the
/// store uses internally.
async fn bucket_request(
    endpoint: &str,
    bucket: &str,
    method: reqwest::Method,
) -> Result<u16, String> {
    let base = endpoint.trim_end_matches('/');
    let url = format!("{base}/{bucket}/");
    let host = url
        .split("://")
        .nth(1)
        .and_then(|h| h.split('/').next())
        .unwrap_or("")
        .to_string();
    let canonical_uri = format!("/{bucket}/");

    let payload_hash = sigv4::EMPTY_PAYLOAD_SHA256.to_string();
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    let headers = vec![
        sigv4::Header::new("host", &host),
        sigv4::Header::new("x-amz-content-sha256", &payload_hash),
        sigv4::Header::new("x-amz-date", &amz_date),
    ];
    let sig_req = sigv4::Request {
        method: method.as_str(),
        canonical_uri: &canonical_uri,
        canonical_query: "",
        headers,
        payload_hash: &payload_hash,
    };
    let creds = sigv4::Credentials {
        access_key: ACCESS_KEY,
        secret_key: SECRET_KEY,
        region: REGION,
        service: "s3",
    };
    let signed = sigv4::sign(&sig_req, &creds, &amz_date, &date_stamp);

    let resp = reqwest::Client::new()
        .request(method, &url)
        .header("host", &host)
        .header("x-amz-content-sha256", &payload_hash)
        .header("x-amz-date", &amz_date)
        .header("authorization", &signed.authorization)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(resp.status().as_u16())
}

/// Creates `bucket` (idempotent: LocalStack returns 200 on re-create).
async fn create_bucket(endpoint: &str, bucket: &str) -> Result<(), String> {
    let code = bucket_request(endpoint, bucket, reqwest::Method::PUT).await?;
    if code == 200 || code == 204 || code == 409 {
        Ok(())
    } else {
        Err(format!("create bucket {bucket}: HTTP {code}"))
    }
}

/// Deletes `bucket` (best-effort cleanup; a missing bucket is ignored).
async fn delete_bucket(endpoint: &str, bucket: &str) {
    let _ = bucket_request(endpoint, bucket, reqwest::Method::DELETE).await;
}

#[tokio::test]
async fn object_round_trip_against_localstack_s3() {
    let Some(endpoint) = s3_endpoint() else {
        eprintln!(
            "skipping object_round_trip_against_localstack_s3: \
             set FIREFLY_TEST_S3_ENDPOINT (e.g. http://localhost:4566) to run"
        );
        return;
    };
    let bucket = unique_bucket("roundtrip");
    create_bucket(&endpoint, &bucket)
        .await
        .expect("create bucket");

    let outcome = run_round_trip(&endpoint, &bucket).await;

    delete_bucket(&endpoint, &bucket).await;
    outcome.expect("object round-trip should succeed");
}

/// The body of the round-trip, separated so the bucket is always deleted.
async fn run_round_trip(endpoint: &str, bucket: &str) -> Result<(), String> {
    let store = store(endpoint, bucket);
    let key = "docs/report/v1";
    let content = b"firefly s3 round-trip payload".to_vec();

    // put
    let n = store
        .put(key, bytes_reader(content.clone()))
        .await
        .map_err(|e| format!("put: {e}"))?;
    if n != content.len() as i64 {
        return Err(format!("put returned {n}, want {}", content.len()));
    }

    // get → content must round-trip exactly
    let mut reader = store.get(key).await.map_err(|e| format!("get: {e}"))?;
    let mut got = Vec::new();
    reader
        .read_to_end(&mut got)
        .await
        .map_err(|e| format!("read body: {e}"))?;
    if got != content {
        return Err("content did not round-trip".into());
    }

    // head → metadata reflects the object size
    let meta = store.head(key).await.map_err(|e| format!("head: {e}"))?;
    if meta.content_length != content.len() as i64 {
        return Err(format!(
            "head content_length {} != {}",
            meta.content_length,
            content.len()
        ));
    }

    // list → the key is enumerated under its prefix
    let keys = store
        .list("docs/", 100)
        .await
        .map_err(|e| format!("list: {e}"))?;
    if !keys.iter().any(|k| k == key) {
        return Err(format!("listed keys {keys:?} missing {key}"));
    }

    // copy → server-side duplicate, also fetchable
    let copy_key = "docs/report/v2";
    store
        .copy(key, copy_key)
        .await
        .map_err(|e| format!("copy: {e}"))?;
    let mut copied = store
        .get(copy_key)
        .await
        .map_err(|e| format!("get copy: {e}"))?;
    let mut copy_body = Vec::new();
    copied
        .read_to_end(&mut copy_body)
        .await
        .map_err(|e| format!("read copy: {e}"))?;
    if copy_body != content {
        return Err("copied content mismatch".into());
    }

    // delete both objects; a follow-up get must 404 (mapped to NotFound)
    store
        .delete(key)
        .await
        .map_err(|e| format!("delete: {e}"))?;
    store
        .delete(copy_key)
        .await
        .map_err(|e| format!("delete copy: {e}"))?;
    match store.get(key).await {
        Err(EcmError::NotFound) => {}
        Err(other) => return Err(format!("expected NotFound after delete, got {other}")),
        Ok(_) => return Err("object still present after delete".into()),
    }

    Ok(())
}
