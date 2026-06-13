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

//! Integration tests ported 1:1 from the Go module's
//! `observability_test.go`, plus Rust-specific coverage for the tracing
//! layer (async scope propagation, span enrichment, level filtering, text
//! format) and the health wire shape.

use std::time::Duration;

use firefly_observability::{
    print_banner, render_banner, subscriber_with_writer, BannerData, BufferWriter, Composite,
    HealthResult, IndicatorFn, LogConfig, LogFormat, Status,
};
use tracing::instrument::WithSubscriber;
use tracing::Level;

fn json_logger(service: &str) -> (BufferWriter, impl tracing::Subscriber + Send + Sync) {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new().with_service(service);
    let sub = subscriber_with_writer(cfg, buf.clone());
    (buf, sub)
}

/// Port of Go `TestLoggerEmitsCorrelationID`.
#[test]
fn logger_emits_correlation_id() {
    let (buf, sub) = json_logger("orders");
    tracing::subscriber::with_default(sub, || {
        firefly_kernel::with_correlation_id_sync("abc-xyz", || {
            tracing::info!("hello");
        });
    });

    let out = buf.as_string();
    let rec: serde_json::Value = serde_json::from_str(out.trim()).expect("not JSON");
    assert_eq!(
        rec["correlationId"], "abc-xyz",
        "missing correlation: {rec}"
    );
    assert_eq!(rec["service"], "orders", "missing service: {rec}");
    assert_eq!(rec["msg"], "hello");
    assert_eq!(rec["level"], "INFO");
    // `time` is an RFC 3339 timestamp, parseable like Go's slog output.
    let time = rec["time"].as_str().expect("time string");
    chrono::DateTime::parse_from_rfc3339(time).expect("RFC 3339 time");
}

/// Rust-specific: the correlation id propagates through a real async
/// task-local scope (the analog of passing ctx through goroutines).
#[tokio::test]
async fn logger_emits_correlation_id_in_async_scope() {
    let (buf, sub) = json_logger("orders");
    firefly_kernel::with_correlation_id("async-123", async {
        tracing::info!(id = "42", "placed order");
    })
    .with_subscriber(sub)
    .await;

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert_eq!(rec["correlationId"], "async-123");
    assert_eq!(rec["id"], "42", "event fields appear top-level: {rec}");
    assert_eq!(rec["msg"], "placed order");
}

/// Rust-specific: no scope, no `correlationId` field — the Go handler
/// only adds the attribute when the context carries one.
#[test]
fn logger_omits_correlation_id_without_scope() {
    let (buf, sub) = json_logger("orders");
    tracing::subscriber::with_default(sub, || {
        tracing::info!("no scope");
    });

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert!(rec.get("correlationId").is_none(), "unexpected: {rec}");
    assert_eq!(rec["service"], "orders");
}

/// Rust-specific: the empty-service config omits the `service` field,
/// like Go's `if cfg.Service != ""` branch.
#[test]
fn logger_omits_empty_service() {
    let buf = BufferWriter::new();
    let sub = subscriber_with_writer(LogConfig::default(), buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::info!("anonymous");
    });

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert!(rec.get("service").is_none(), "unexpected: {rec}");
}

/// Rust-specific: the active span's `trace_id`/`span_id` are injected into
/// every log record from the W3C trace-context scope — the analog of pyfly's
/// `_add_trace_ids` structlog processor (the SLF4J MDC equivalent).
#[tokio::test]
async fn logger_emits_trace_and_span_ids_from_trace_context() {
    let (buf, sub) = json_logger("orders");
    firefly_observability::with_trace_context(
        Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string()),
        None,
        async {
            tracing::info!("served");
        },
    )
    .with_subscriber(sub)
    .await;

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert_eq!(
        rec["trace_id"], "0af7651916cd43dd8448eb211c80319c",
        "missing trace_id: {rec}"
    );
    assert_eq!(rec["span_id"], "b7ad6b7169203331", "missing span_id: {rec}");
    assert_eq!(rec["msg"], "served");
}

/// Rust-specific: with no trace context in scope, no `trace_id`/`span_id`
/// fields appear — the injection stays a no-op without tracing, matching
/// pyfly's "no active span" branch.
#[test]
fn logger_omits_trace_ids_without_trace_context() {
    let (buf, sub) = json_logger("orders");
    tracing::subscriber::with_default(sub, || {
        tracing::info!("no trace");
    });

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert!(rec.get("trace_id").is_none(), "unexpected trace_id: {rec}");
    assert!(rec.get("span_id").is_none(), "unexpected span_id: {rec}");
}

/// Rust-specific: text format mirrors slog's text handler — key=value
/// pairs with the same field names.
#[test]
fn logger_text_format_keeps_field_names() {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new()
        .with_service("orders")
        .with_format(LogFormat::from_name("text"));
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        firefly_kernel::with_correlation_id_sync("abc-xyz", || {
            tracing::warn!("cold start");
        });
    });

    let out = buf.as_string();
    assert!(out.contains("level=WARN"), "{out}");
    assert!(out.contains(r#"msg="cold start""#), "{out}");
    assert!(out.contains("service=orders"), "{out}");
    assert!(out.contains("correlationId=abc-xyz"), "{out}");
    assert!(out.contains("time="), "{out}");
}

/// Rust-specific: events below the configured level are dropped.
#[test]
fn logger_respects_level_filter() {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new().with_level(Level::INFO);
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::debug!("dropped");
        tracing::info!("kept");
        tracing::error!("also kept");
    });

    let out = buf.as_string();
    assert!(!out.contains("dropped"), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(first["level"], "INFO");
    assert_eq!(second["level"], "ERROR");
}

/// Rust-specific: fields recorded on enclosing spans enrich every event —
/// the analog of Go's `logger.With("tenant", …)`.
#[test]
fn logger_merges_span_fields() {
    let (buf, sub) = json_logger("orders");
    tracing::subscriber::with_default(sub, || {
        let span = tracing::info_span!("request", tenant = "acme");
        let _guard = span.enter();
        tracing::info!("inside span");
    });

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert_eq!(rec["tenant"], "acme", "span field missing: {rec}");
    assert_eq!(rec["msg"], "inside span");
}

/// Port of Go `TestCompositeHealth`.
#[tokio::test]
async fn composite_health() {
    let c = Composite::new();
    c.add(IndicatorFn::new("db", || async { HealthResult::up() }));
    c.add(IndicatorFn::new("cache", || async {
        HealthResult::degraded("cold start")
    }));
    let (overall, m) = c.check_all().await;
    assert_eq!(overall, Status::Degraded, "overall: {overall}");
    assert_eq!(m.len(), 2, "results");

    c.add(IndicatorFn::new("broker", || async {
        HealthResult::down("disconnected")
    }));
    let (overall, _) = c.check_all().await;
    assert_eq!(overall, Status::Down, "overall after down: {overall}");
}

/// Rust-specific: `check_all` stamps duration and start time, and the
/// JSON wire shape matches the Go struct tags.
#[tokio::test]
async fn composite_stamps_duration_and_time_with_go_wire_shape() {
    let before = chrono::Utc::now();
    let c = Composite::new();
    c.add(IndicatorFn::new("db", || async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        HealthResult::up()
    }));
    let (_, results) = c.check_all().await;
    let r = &results["db"];
    assert!(r.duration >= Duration::from_millis(10), "{:?}", r.duration);
    assert!(r.time >= before && r.time <= chrono::Utc::now());

    let value = serde_json::to_value(r).unwrap();
    let obj = value.as_object().unwrap();
    assert_eq!(obj["status"], "UP");
    assert!(!obj.contains_key("message"));
    assert!(!obj.contains_key("details"));
    assert!(obj["duration"].as_i64().unwrap() >= 10_000_000); // nanoseconds
    assert!(obj["time"].is_string());
}

/// Rust-specific: details serialize under `details`, message under
/// `message` — the exact Go field names.
#[tokio::test]
async fn health_result_details_and_message_field_names() {
    let c = Composite::new();
    c.add(IndicatorFn::new("pool", || async {
        HealthResult::degraded("saturated").with_detail("inUse", 19)
    }));
    let (_, results) = c.check_all().await;
    let value = serde_json::to_value(&results["pool"]).unwrap();
    assert_eq!(value["status"], "DEGRADED");
    assert_eq!(value["message"], "saturated");
    assert_eq!(value["details"]["inUse"], 19);
}

/// Port of Go `TestBannerContents`.
#[test]
fn banner_contents() {
    let mut buf = Vec::new();
    print_banner(&mut buf, "starter-core", "orders-service").unwrap();
    let out = String::from_utf8(buf).unwrap();
    for want in [
        "Firefly Framework for Rust",
        firefly_kernel::VERSION,
        "starter-core",
        "orders-service",
    ] {
        assert!(out.contains(want), "banner missing {want:?} in:\n{out}");
    }
    // The Rust toolchain version must appear so operators reading the
    // banner know which compiler shipped the binary.
    assert!(
        out.contains("Rust "),
        "banner missing Rust runtime line:\n{out}"
    );
}

/// Port of Go `TestRenderBannerOverrides`.
#[test]
fn render_banner_overrides() {
    let mut buf = Vec::new();
    render_banner(
        &mut buf,
        BannerData {
            version: "99.99.99".into(),
            starter: "custom-starter".into(),
            app: "custom-app".into(),
            rust_version: "1.99.0".into(),
        },
    )
    .unwrap();
    let out = String::from_utf8(buf).unwrap();
    for want in ["99.99.99", "custom-starter", "custom-app", "Rust 1.99.0"] {
        assert!(out.contains(want), "override missed {want:?} in:\n{out}");
    }
}

/// Port of Go `TestBannerEmbeddedFromTxt`: guards against drift — if
/// someone changes the embedded banner.txt, this confirms the rendered
/// output still contains the canonical ASCII art header.
#[test]
fn banner_embedded_from_txt() {
    let mut buf = Vec::new();
    print_banner(&mut buf, "starter-core", "orders").unwrap();
    let out = String::from_utf8(buf).unwrap();
    // The `_/ ____\__|` fragment is the second line of the ASCII art —
    // stable across cosmetic edits to the version line.
    assert!(
        out.contains(r"_/ ____\__|"),
        "ASCII art header missing — banner.txt may have drifted:\n{out}"
    );
}
