//! Env-gated live integration test for [`firefly_ecm_storage_azure::BlobStore`]
//! against a real Azure-Blob-compatible endpoint (Azurite).
//!
//! This is an **env-gated** integration test, not `#[ignore]`-gated. It reads
//! `FIREFLY_TEST_AZURITE_URL` (e.g. `http://localhost:10000`); when that
//! variable is **unset** it prints a one-line `skipping …` and returns, so
//! `cargo test` on a bare machine is green. When it is **set** the test creates
//! a fresh container, then exercises the blob round-trip through the store
//! (put → get → list → properties → delete) and finally deletes the container.
//!
//! Run against the docker-compose stack with:
//!
//! ```sh
//! export FIREFLY_TEST_AZURITE_URL="http://localhost:10000"
//! cargo test -p firefly-ecm-storage-azure --test blob_integration
//! ```
//!
//! Container and blob names are derived from the test fn name, the process id,
//! and a process-wide atomic counter (never a random source), so concurrent
//! runs against one Azurite never collide and every test cleans up after
//! itself.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use firefly_ecm::{bytes_reader, ContentStore, EcmError};
use firefly_ecm_storage_azure::{sharedkey, BlobStore, Config};
use tokio::io::AsyncReadExt;

/// The well-known Azurite development account + key (safe to embed; they are
/// the public emulator credentials).
const ACCOUNT: &str = "devstoreaccount1";
const KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
const X_MS_VERSION: &str = "2021-08-06";

/// Process-wide monotonic counter for unique container/blob names.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the Azurite base URL from the standard env var. Returns `None` when
/// unset so callers can early-skip.
fn azurite_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_AZURITE_URL")
        .ok()
        .filter(|s| !s.is_empty())
}

/// The store endpoint for Azurite — the base URL with the account segment, the
/// form `BlobStore` expects (`{endpoint}/{container}/{blob}`).
fn store_endpoint(base: &str) -> String {
    format!("{}/{ACCOUNT}", base.trim_end_matches('/'))
}

/// A unique, DNS-safe container name for this `slug` (lowercase, hyphenated).
fn unique_container(slug: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("fftest-blob-{slug}-{}-{n}", std::process::id())
}

/// Builds a store targeting `container` on the integration endpoint.
fn store(base: &str, container: &str) -> BlobStore {
    BlobStore::new(Config {
        account: ACCOUNT.into(),
        key: KEY.into(),
        container: container.into(),
        endpoint: store_endpoint(base),
        ..Default::default()
    })
    .unwrap()
}

/// Issues a Shared-Key-signed container-level request (`PUT` to create,
/// `DELETE` to remove) against Azurite, returning the HTTP status code.
///
/// Container lifecycle is not part of the `ContentStore` port, so the test
/// signs these two calls directly with the crate's public [`sharedkey`]
/// signer — the same code path the store uses internally. The `restype`
/// container query param folds into the canonical resource as a sorted
/// `\nrestype:container` line.
async fn container_request(
    base: &str,
    container: &str,
    method: reqwest::Method,
) -> Result<u16, String> {
    let endpoint = store_endpoint(base);
    let url = format!("{endpoint}/{container}?restype=container");
    let host = endpoint
        .split("://")
        .nth(1)
        .and_then(|h| h.split('/').next())
        .unwrap_or("")
        .to_string();
    // The Shared Key canonical resource is `/<account>` + the request URL's
    // path. Azurite is reached path-style, so the URL path already contains the
    // account segment (`store_endpoint` appends `/devstoreaccount1`), giving the
    // doubled-account form `/devstoreaccount1/devstoreaccount1/<container>` —
    // exactly what Azurite recomputes server-side.
    let canonical_resource = format!("/{ACCOUNT}/{ACCOUNT}/{container}\nrestype:container");

    let now = Utc::now();
    let x_ms_date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    let x_ms_headers = vec![
        sharedkey::Header::new("x-ms-date", &x_ms_date),
        sharedkey::Header::new("x-ms-version", X_MS_VERSION),
    ];
    let sig_req = sharedkey::Request {
        method: method.as_str(),
        content_length: "",
        content_type: "",
        x_ms_headers,
        canonical_resource: &canonical_resource,
    };
    let (authorization, _sig, _sts) =
        sharedkey::sign(&sig_req, ACCOUNT, KEY).map_err(|e| format!("sign: {e}"))?;

    let resp = reqwest::Client::new()
        .request(method, &url)
        .header("host", &host)
        .header("x-ms-date", &x_ms_date)
        .header("x-ms-version", X_MS_VERSION)
        .header("authorization", &authorization)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(resp.status().as_u16())
}

/// Creates `container` (idempotent: a 409 "already exists" is treated as OK).
async fn create_container(base: &str, container: &str) -> Result<(), String> {
    let code = container_request(base, container, reqwest::Method::PUT).await?;
    if code == 201 || code == 200 || code == 409 {
        Ok(())
    } else {
        Err(format!("create container {container}: HTTP {code}"))
    }
}

/// Deletes `container` (best-effort cleanup; a missing container is ignored).
async fn delete_container(base: &str, container: &str) {
    let _ = container_request(base, container, reqwest::Method::DELETE).await;
}

#[tokio::test]
async fn blob_round_trip_against_azurite() {
    let Some(base) = azurite_url() else {
        eprintln!(
            "skipping blob_round_trip_against_azurite: \
             set FIREFLY_TEST_AZURITE_URL (e.g. http://localhost:10000) to run"
        );
        return;
    };
    let container = unique_container("roundtrip");
    create_container(&base, &container)
        .await
        .expect("create container");

    let outcome = run_round_trip(&base, &container).await;

    delete_container(&base, &container).await;
    outcome.expect("blob round-trip should succeed");
}

/// The body of the round-trip, separated so the container is always deleted.
async fn run_round_trip(base: &str, container: &str) -> Result<(), String> {
    let store = store(base, container);
    let key = "docs/report/v1";
    let content = b"firefly azure blob round-trip payload".to_vec();

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

    // properties → metadata reflects the blob size
    let props = store
        .properties(key)
        .await
        .map_err(|e| format!("properties: {e}"))?;
    if props.content_length != content.len() as i64 {
        return Err(format!(
            "properties content_length {} != {}",
            props.content_length,
            content.len()
        ));
    }

    // list → the blob name is enumerated under its prefix
    let names = store
        .list("docs/")
        .await
        .map_err(|e| format!("list: {e}"))?;
    if !names.iter().any(|k| k == key) {
        return Err(format!("listed names {names:?} missing {key}"));
    }

    // delete → a follow-up get must 404 (mapped to NotFound)
    store
        .delete(key)
        .await
        .map_err(|e| format!("delete: {e}"))?;
    match store.get(key).await {
        Err(EcmError::NotFound) => {}
        Err(other) => return Err(format!("expected NotFound after delete, got {other}")),
        Ok(_) => return Err("blob still present after delete".into()),
    }

    Ok(())
}
