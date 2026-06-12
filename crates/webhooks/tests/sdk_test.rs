// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! SDK forwarder tests. The Go module ships `sdk.Client` untested; the
//! Rust port proves the documented contract end-to-end: a forwarded
//! payload arrives byte-identical with its signature headers and passes
//! validation on a live ingestion endpoint.

mod common;

use std::sync::Arc;

use common::{spawn_server, CaptureProcessor};
use firefly_testkit::{sign_github, sign_hmac};
use firefly_webhooks::{web, Client, GitHubValidator, HmacValidator, MemoryDlq, Pipeline};

fn github_pipeline(secret: &[u8]) -> (Arc<Pipeline>, Arc<CaptureProcessor>) {
    let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
    pipeline.register_validator(GitHubValidator::new(secret));
    let proc = CaptureProcessor::new("github");
    pipeline.register_processor(proc.clone());
    (pipeline, proc)
}

#[tokio::test]
async fn forward_round_trips_through_the_ingestion_endpoint() {
    let secret = b"gh_secret";
    let (pipeline, proc) = github_pipeline(secret);
    let base = spawn_server(web::router(pipeline)).await;

    let payload: &[u8] = br#"{"action":"opened"}"#;
    let sig = sign_github(secret, payload);

    let client = Client::new(&base);
    client
        .forward("github", payload, &[("X-Hub-Signature-256", &sig)])
        .await
        .expect("forward succeeds");

    assert_eq!(proc.hits(), 1);
    let ev = proc.last().expect("event captured");
    // The payload crossed the wire byte-identical — the signature
    // verified against the exact bytes the SDK sent.
    assert_eq!(ev.payload, payload);
    assert_eq!(ev.headers.get("X-Hub-Signature-256"), Some(&sig));
}

#[tokio::test]
async fn forward_surfaces_unknown_provider_as_404() {
    let (pipeline, _proc) = github_pipeline(b"gh_secret");
    let base = spawn_server(web::router(pipeline)).await;

    let err = Client::new(&base)
        .forward("missing", b"{}", &[])
        .await
        .expect_err("unknown provider");
    assert_eq!(err.status(), Some(404), "err: {err}");
}

#[tokio::test]
async fn forward_surfaces_bad_signature_as_401() {
    let (pipeline, proc) = github_pipeline(b"gh_secret");
    let base = spawn_server(web::router(pipeline)).await;

    let err = Client::new(&base)
        .forward("github", b"{}", &[("X-Hub-Signature-256", "sha256=bad")])
        .await
        .expect_err("bad signature");
    assert_eq!(err.status(), Some(401), "err: {err}");
    let fe = err.as_firefly().expect("firefly error");
    assert_eq!(fe.detail, "firefly/webhooks: signature mismatch\n");
    assert_eq!(proc.hits(), 0);
}

#[tokio::test]
async fn forward_replays_a_dead_lettered_event() {
    // First delivery fails into the DLQ on service A …
    let secret = b"s3cret";
    let dlq = Arc::new(MemoryDlq::new());
    let failing = Arc::new(Pipeline::new(dlq.clone()));
    failing.register_validator(HmacValidator::new("generic", secret));
    failing.register_processor(CaptureProcessor::failing("generic", "downstream offline"));

    let payload: &[u8] = br#"{"x":1}"#;
    let sig = sign_hmac(secret, payload);
    let base_a = spawn_server(web::router(failing)).await;
    // A 500 is retryable (like the framework REST client), so cap the
    // budget at one attempt to keep the failure — and the DLQ — single.
    Client::new(&base_a)
        .with_retries(1)
        .forward("generic", payload, &[("X-Signature", &sig)])
        .await
        .expect_err("processor failure surfaces as 500");
    assert_eq!(dlq.len(), 1);

    // … then the DLQ entry is replayed to a healthy service B, reusing
    // the recorded payload and signature header.
    let (healthy, proc) = {
        let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
        pipeline.register_validator(HmacValidator::new("generic", secret));
        let proc = CaptureProcessor::new("generic");
        pipeline.register_processor(proc.clone());
        (pipeline, proc)
    };
    let base_b = spawn_server(web::router(healthy)).await;

    let entry = &dlq.entries()[0];
    let recorded_sig = entry.event.headers["X-Signature"].clone();
    Client::new(&base_b)
        .forward(
            &entry.event.provider,
            &entry.event.payload,
            &[("X-Signature", &recorded_sig)],
        )
        .await
        .expect("replay succeeds");
    assert_eq!(proc.hits(), 1);
}
