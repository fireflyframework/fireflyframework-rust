//! Pipeline + DLQ tests, ported 1:1 from the Go module's
//! `core/pipeline_test.go` plus Rust-specific coverage.

mod common;

use std::sync::Arc;

use common::{inbound, CaptureProcessor};
use firefly_webhooks::{HmacValidator, MemoryDlq, Pipeline};

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
