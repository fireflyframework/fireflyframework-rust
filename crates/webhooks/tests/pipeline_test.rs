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

//! Pipeline + DLQ tests, ported 1:1 from the Go module's
//! `core/pipeline_test.go` plus Rust-specific coverage.

mod common;

use std::sync::Arc;

use common::{inbound, CaptureProcessor};
use firefly_webhooks::{
    EventStore, HmacValidator, Inbound, MemoryDlq, MemoryEventStore, Pipeline, WebhookError,
    DEFAULT_IDEMPOTENCY_HEADER,
};

/// Builds an [`Inbound`] for `provider`/`event_type` carrying an
/// idempotency key under the given (canonical-MIME) header.
fn inbound_with_key(provider: &str, event_type: &str, header: &str, key: &str) -> Inbound {
    let mut ev = inbound(provider, event_type);
    ev.headers.insert(header.to_owned(), key.to_owned());
    ev
}

// --- Go: TestPipelineDLQ -----------------------------------------------------

#[tokio::test]
async fn pipeline_pushes_failed_event_to_dlq() {
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Pipeline::new(dlq.clone());
    pipeline.register_processor(CaptureProcessor::failing("stripe", "boom"));

    let err = pipeline
        .process(inbound("stripe", "x"))
        .await
        .expect_err("expected error");
    assert_eq!(err.to_string(), "boom");

    let entries = dlq.entries();
    assert_eq!(entries.len(), 1, "dlq: {entries:?}");
    assert_eq!(entries[0].err, "boom");
    assert_eq!(entries[0].event.provider, "stripe");
    assert_eq!(entries[0].event.event_type, "x");
}

// --- Go: TestPipelineEnrichesAndProcesses ------------------------------------

#[tokio::test]
async fn pipeline_enriches_and_processes() {
    let pipeline = Pipeline::without_dlq();
    pipeline.enrich(|ev| ev.event_type = format!("{}:enriched", ev.event_type));

    let proc = CaptureProcessor::new("github");
    pipeline.register_processor(proc.clone());

    pipeline
        .process(inbound("github", "push"))
        .await
        .expect("process");
    let seen = proc.last().expect("processor ran");
    assert_eq!(seen.event_type, "push:enriched");
}

// --- Rust-specific coverage --------------------------------------------------

#[tokio::test]
async fn pipeline_aborts_downstream_processors_on_error() {
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Pipeline::new(dlq.clone());

    let first = CaptureProcessor::failing("stripe", "boom");
    let second = CaptureProcessor::new("stripe");
    pipeline.register_processor(first.clone());
    pipeline.register_processor(second.clone());

    pipeline
        .process(inbound("stripe", "x"))
        .await
        .expect_err("first processor fails");

    assert_eq!(first.hits(), 1);
    assert_eq!(second.hits(), 0, "downstream processor must not run");
    assert_eq!(dlq.len(), 1);
}

#[tokio::test]
async fn pipeline_dead_letters_the_enriched_event() {
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Pipeline::new(dlq.clone());
    pipeline.enrich(|ev| ev.event_type = format!("{}:enriched", ev.event_type));
    pipeline.register_processor(CaptureProcessor::failing("stripe", "boom"));

    pipeline
        .process(inbound("stripe", "x"))
        .await
        .expect_err("fails");
    assert_eq!(dlq.entries()[0].event.event_type, "x:enriched");
}

#[tokio::test]
async fn pipeline_without_dlq_still_returns_the_error() {
    let pipeline = Pipeline::without_dlq();
    pipeline.register_processor(CaptureProcessor::failing("stripe", "boom"));
    let err = pipeline
        .process(inbound("stripe", "x"))
        .await
        .expect_err("error surfaces without a dlq");
    assert_eq!(err.to_string(), "boom");
}

#[tokio::test]
async fn pipeline_ignores_events_for_unregistered_providers() {
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Pipeline::new(dlq.clone());
    let proc = CaptureProcessor::new("github");
    pipeline.register_processor(proc.clone());

    pipeline
        .process(inbound("stripe", "x"))
        .await
        .expect("no processors for stripe → success");
    assert_eq!(proc.hits(), 0);
    assert!(dlq.is_empty());
}

#[tokio::test]
async fn validators_returns_a_copy_of_the_registered_map() {
    let pipeline = Pipeline::without_dlq();
    pipeline.register_validator(HmacValidator::new("generic", b"s3cret"));

    let snapshot = pipeline.validators();
    assert!(snapshot.contains_key("generic"));
    assert_eq!(snapshot["generic"].provider(), "generic");

    // A later registration does not mutate the earlier snapshot.
    pipeline.register_validator(HmacValidator::new("other", b"k"));
    assert!(!snapshot.contains_key("other"));
    assert!(pipeline.validators().contains_key("other"));
}

#[tokio::test]
async fn memory_dlq_starts_empty() {
    let dlq = MemoryDlq::new();
    assert!(dlq.is_empty());
    assert_eq!(dlq.len(), 0);
    assert!(dlq.entries().is_empty());
}

// --- pyfly parity: idempotency EventStore dedup ------------------------------

// --- pyfly: test_processor_dedupes_idempotency_keys --------------------------

#[tokio::test]
async fn pipeline_dedupes_idempotency_keys() {
    let store = Arc::new(MemoryEventStore::new());
    let pipeline = Pipeline::without_dlq();
    pipeline.register_event_store_arc(store.clone());
    let proc = CaptureProcessor::new("stripe");
    pipeline.register_processor(proc.clone());

    // First delivery dispatches and records the key.
    pipeline
        .process(inbound_with_key(
            "stripe",
            "x",
            DEFAULT_IDEMPOTENCY_HEADER,
            "abc",
        ))
        .await
        .expect("first delivery dispatches");
    // Redelivery (same key) is skipped — Ok, but no second dispatch.
    pipeline
        .process(inbound_with_key(
            "stripe",
            "x",
            DEFAULT_IDEMPOTENCY_HEADER,
            "abc",
        ))
        .await
        .expect("duplicate is a no-op success");

    assert_eq!(proc.hits(), 1, "processor ran exactly once");
    assert!(store.already_processed("abc").await.unwrap());
}

#[tokio::test]
async fn pipeline_without_event_store_never_dedupes() {
    let pipeline = Pipeline::without_dlq();
    let proc = CaptureProcessor::new("stripe");
    pipeline.register_processor(proc.clone());

    for _ in 0..3 {
        pipeline
            .process(inbound_with_key(
                "stripe",
                "x",
                DEFAULT_IDEMPOTENCY_HEADER,
                "abc",
            ))
            .await
            .expect("dispatch");
    }
    assert_eq!(proc.hits(), 3, "no store → no dedup");
}

#[tokio::test]
async fn pipeline_dispatches_events_without_an_idempotency_header() {
    let store = Arc::new(MemoryEventStore::new());
    let pipeline = Pipeline::without_dlq();
    pipeline.register_event_store_arc(store.clone());
    let proc = CaptureProcessor::new("stripe");
    pipeline.register_processor(proc.clone());

    // No idempotency header: each delivery dispatches, nothing is recorded.
    pipeline.process(inbound("stripe", "x")).await.expect("ok");
    pipeline.process(inbound("stripe", "x")).await.expect("ok");
    assert_eq!(proc.hits(), 2);
    assert!(store.is_empty(), "no key was recorded");
}

#[tokio::test]
async fn pipeline_dedupes_distinct_keys_independently() {
    let store = Arc::new(MemoryEventStore::new());
    let pipeline = Pipeline::without_dlq();
    pipeline.register_event_store_arc(store);
    let proc = CaptureProcessor::new("stripe");
    pipeline.register_processor(proc.clone());

    for key in ["a", "b", "a", "c", "b"] {
        pipeline
            .process(inbound_with_key(
                "stripe",
                "x",
                DEFAULT_IDEMPOTENCY_HEADER,
                key,
            ))
            .await
            .expect("dispatch");
    }
    // a, b, c each dispatched once; the repeats were skipped.
    assert_eq!(proc.hits(), 3);
}

#[tokio::test]
async fn pipeline_honours_a_custom_idempotency_header() {
    let pipeline = Pipeline::without_dlq();
    pipeline.register_event_store(MemoryEventStore::new());
    pipeline.with_idempotency_header("X-Dedup-Id");
    let proc = CaptureProcessor::new("stripe");
    pipeline.register_processor(proc.clone());

    pipeline
        .process(inbound_with_key("stripe", "x", "X-Dedup-Id", "k1"))
        .await
        .expect("dispatch");
    pipeline
        .process(inbound_with_key("stripe", "x", "X-Dedup-Id", "k1"))
        .await
        .expect("duplicate skipped");
    // The default header is now ignored, so a key under it is not deduped.
    pipeline
        .process(inbound_with_key(
            "stripe",
            "x",
            DEFAULT_IDEMPOTENCY_HEADER,
            "k1",
        ))
        .await
        .expect("dispatch");

    assert_eq!(proc.hits(), 2);
}

#[tokio::test]
async fn pipeline_deduplicates_before_processor_dispatch() {
    // A failing processor proves dedup short-circuits *before* dispatch:
    // the second delivery must not reach the (failing) processor at all.
    let dlq = Arc::new(MemoryDlq::new());
    let pipeline = Pipeline::new(dlq.clone());
    pipeline.register_event_store(MemoryEventStore::new());
    pipeline.register_processor(CaptureProcessor::failing("stripe", "boom"));

    let err = pipeline
        .process(inbound_with_key(
            "stripe",
            "x",
            DEFAULT_IDEMPOTENCY_HEADER,
            "dup",
        ))
        .await
        .expect_err("first delivery hits the failing processor");
    assert_eq!(err.to_string(), "boom");
    assert_eq!(dlq.len(), 1);

    // The key was recorded before dispatch, so the redelivery is skipped
    // and never reaches the processor — no second DLQ entry, Ok result.
    pipeline
        .process(inbound_with_key(
            "stripe",
            "x",
            DEFAULT_IDEMPOTENCY_HEADER,
            "dup",
        ))
        .await
        .expect("duplicate skipped, no dispatch");
    assert_eq!(dlq.len(), 1, "duplicate did not reach the processor");
}

#[tokio::test]
async fn pipeline_surfaces_event_store_lookup_failures_fail_closed() {
    // A store whose lookup always fails must abort the pipeline (the
    // event is not dispatched) rather than silently dispatch.
    struct FailingStore;

    #[async_trait::async_trait]
    impl EventStore for FailingStore {
        async fn already_processed(&self, _key: &str) -> Result<bool, WebhookError> {
            Err(WebhookError::processor("store down"))
        }
        async fn remember(&self, _key: &str) -> Result<(), WebhookError> {
            Ok(())
        }
    }

    let pipeline = Pipeline::without_dlq();
    pipeline.register_event_store(FailingStore);
    let proc = CaptureProcessor::new("stripe");
    pipeline.register_processor(proc.clone());

    let err = pipeline
        .process(inbound_with_key(
            "stripe",
            "x",
            DEFAULT_IDEMPOTENCY_HEADER,
            "k",
        ))
        .await
        .expect_err("lookup failure aborts");
    assert_eq!(err.to_string(), "store down");
    assert_eq!(proc.hits(), 0, "fail-closed: no dispatch");
}
