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

//! Integration tests for the pyfly-parity surface, ported from
//! `tests/observability/` (metrics idempotency, timed/counted Micrometer
//! naming, W3C trace propagation) and `tests/logging/` (redaction engine +
//! processor, handlers/parse_size, per-logger levels) in the pyfly repo.

use std::convert::Infallible;
use std::sync::Arc;

use firefly_observability::{
    apply_external_config, counted_result, current_traceparent, current_tracestate, inject_headers,
    inject_reqwest, load_log_config, subscriber_with_writer, subscriber_with_writer_and_handle,
    timed, with_trace_context, BufferWriter, Counted, FileConfig, LogConfig, LogFormat, MaskStyle,
    MetricsRegistry, ProcessMetricsCollector, RedactionConfig, Timed, TraceContextLayer,
    TraceParent, TraceState, TRACEPARENT_HEADER, TRACESTATE_HEADER,
};
use http::{HeaderMap, Request, Response};
use tower::{Layer, Service, ServiceExt};
use tracing::Level;

// ---------------------------------------------------------------------------
// metrics — pyfly tests/observability/test_metrics_idempotent.py
// ---------------------------------------------------------------------------

/// pyfly `test_two_registries_share_the_same_counter` (+histogram/+gauge):
/// the exact scenario that used to raise "Duplicated timeseries".
#[test]
fn two_registries_share_the_same_collectors() {
    let first = MetricsRegistry::new();
    let second = MetricsRegistry::new();

    let c1 = first.counter("pyfly_test_idempotent_counter_total", "first", &["route"]);
    let c2 = second.counter("pyfly_test_idempotent_counter_total", "second", &["route"]);
    assert!(Arc::ptr_eq(&c1, &c2));

    let h1 = first.histogram(
        "pyfly_test_idempotent_histogram_seconds",
        "first",
        &["op"],
        Some(&[0.1, 0.5]),
    );
    let h2 = second.histogram(
        "pyfly_test_idempotent_histogram_seconds",
        "second",
        &["op"],
        Some(&[0.1, 0.5]),
    );
    assert!(Arc::ptr_eq(&h1, &h2));

    let g1 = first.gauge("pyfly_test_idempotent_gauge", "first", &["pool"]);
    let g2 = second.gauge("pyfly_test_idempotent_gauge", "second", &["pool"]);
    assert!(Arc::ptr_eq(&g1, &g2));
}

/// pyfly `test_second_registry_does_not_raise_duplicated_timeseries`.
#[test]
fn second_registry_does_not_panic_on_duplicates() {
    let first = MetricsRegistry::new();
    let second = MetricsRegistry::new();
    first.counter("pyfly_test_dup_counter_total", "c", &["a"]);
    first.histogram("pyfly_test_dup_histogram_seconds", "h", &["a"], None);
    first.gauge("pyfly_test_dup_gauge", "g", &["a"]);
    second.counter("pyfly_test_dup_counter_total", "c", &["a"]);
    second.histogram("pyfly_test_dup_histogram_seconds", "h", &["a"], None);
    second.gauge("pyfly_test_dup_gauge", "g", &["a"]);
}

// ---------------------------------------------------------------------------
// metrics — pyfly tests/observability/test_observability.py::TestMetrics
// ---------------------------------------------------------------------------

/// pyfly `test_registry_creates_counter`.
#[test]
fn registry_creates_counter() {
    let registry = MetricsRegistry::isolated();
    let counter = registry.counter("test_requests_total", "Total requests", &[]);
    counter.inc();
    assert_eq!(counter.value(), 1.0);
}

/// pyfly `test_registry_creates_histogram`.
#[test]
fn registry_creates_histogram() {
    let registry = MetricsRegistry::isolated();
    let histogram = registry.histogram("test_duration_seconds", "Request duration", &[], None);
    histogram.observe(0.5);
    assert_eq!(histogram.count(), 1);
}

/// pyfly `test_registry_creates_gauge`.
#[test]
fn registry_creates_gauge() {
    let registry = MetricsRegistry::isolated();
    let gauge = registry.gauge("test_gauge_active_connections", "Active connections", &[]);
    gauge.inc();
    gauge.inc();
    gauge.dec();
    assert_eq!(gauge.value(), 1.0);
}

/// pyfly `test_timed_decorator_micrometer_naming_and_tags`: Micrometer
/// dot.case name -> Prometheus `<name>_seconds` timer with
/// class/method/exception tags (label pairs sorted alphabetically in the
/// exposition, like prometheus_client).
#[tokio::test]
async fn timed_micrometer_naming_and_tags() {
    let registry = MetricsRegistry::isolated();

    let out = Timed::new(&registry, "operation.duration")
        .description("Operation duration")
        .method("slow_operation")
        .record(async { "done" })
        .await;
    assert_eq!(out, "done");

    let exposition = registry.prometheus_text();
    assert!(
        exposition.contains("operation_duration_seconds_count{"),
        "{exposition}"
    );
    assert!(
        exposition.contains(r#"method="slow_operation""#),
        "{exposition}"
    );
    assert!(exposition.contains(r#"exception="none""#), "{exposition}");
    assert!(
        exposition.contains(
            r#"operation_duration_seconds_count{class="",exception="none",method="slow_operation"} 1.0"#
        ),
        "{exposition}"
    );
}

/// pyfly `test_counted_decorator_success_and_failure`.
#[tokio::test]
async fn counted_success_and_failure() {
    #[derive(Debug)]
    struct RuntimeError;

    let registry = MetricsRegistry::isolated();

    let run = |fail: bool| async move {
        if fail {
            Err(RuntimeError)
        } else {
            Ok("ok")
        }
    };

    let counted_call = |fail: bool| {
        Counted::new(&registry, "operation.calls")
            .description("Operation calls")
            .method("my_operation")
            .record_result(run(fail))
    };

    counted_call(false).await.unwrap();
    counted_call(false).await.unwrap();

    // The counter is registered without the `_total` suffix (the
    // exposition appends it, like prometheus_client).
    let counter = registry.counter("operation_calls", "Operation calls", &[]);
    let success = counter.value_with(&["", "my_operation", "success", "none"]);
    assert_eq!(success, 2.0);

    assert!(counted_call(true).await.is_err());
    let failure = counter.value_with(&["", "my_operation", "failure", "RuntimeError"]);
    assert_eq!(failure, 1.0);

    let exposition = registry.prometheus_text();
    assert!(
        exposition.contains("operation_calls_total{"),
        "{exposition}"
    );
    assert!(
        exposition.contains(r#"exception="RuntimeError""#),
        "{exposition}"
    );
}

/// The free helper fns wrap futures directly (the decorator shorthand).
#[tokio::test]
async fn timed_and_counted_helper_fns() {
    let registry = MetricsRegistry::isolated();

    let out = timed(&registry, "orders.process", async { 21 * 2 }).await;
    assert_eq!(out, 42);
    let h = registry.histogram(
        "orders_process_seconds",
        "",
        &["class", "method", "exception"],
        None,
    );
    assert_eq!(h.count_with(&["", "", "none"]), 1);

    let res: Result<u8, std::io::Error> =
        counted_result(&registry, "orders.created", async { Ok(1) }).await;
    assert!(res.is_ok());
    let c = registry.counter("orders_created", "", &[]);
    assert_eq!(c.value_with(&["", "", "success", "none"]), 1.0);
}

// ---------------------------------------------------------------------------
// trace context — pyfly tests/observability/test_trace_propagation.py
// ---------------------------------------------------------------------------

const INBOUND_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";

fn traceparent() -> String {
    format!("00-{INBOUND_TRACE}-b7ad6b7169203331-01")
}

/// pyfly `test_inject_extract_round_trip_preserves_trace`: a context bound
/// on the "server" side is injected verbatim into outbound headers, so the
/// downstream service receives an unbroken chain.
#[tokio::test]
async fn inject_extract_round_trip_preserves_trace() {
    let headers = with_trace_context(Some(traceparent()), Some("vendor=x".to_string()), async {
        let mut headers = HeaderMap::new();
        inject_headers(&mut headers);
        headers
    })
    .await;

    let tp = headers
        .get(TRACEPARENT_HEADER)
        .expect("traceparent injected")
        .to_str()
        .unwrap();
    assert_eq!(tp, traceparent());
    let parsed = TraceParent::parse(tp).unwrap();
    assert_eq!(parsed.trace_id, INBOUND_TRACE);
    assert_eq!(
        headers.get(TRACESTATE_HEADER).unwrap().to_str().unwrap(),
        "vendor=x"
    );
}

/// Outside a scope, injection is a no-op (pyfly: no active span).
#[tokio::test]
async fn inject_without_scope_is_noop() {
    assert!(current_traceparent().is_none());
    assert!(current_tracestate().is_none());
    let mut headers = HeaderMap::new();
    inject_headers(&mut headers);
    assert!(headers.is_empty());
}

/// pyfly `test_tracing_filter_inherits_inbound_trace`: the tower layer
/// extracts the inbound headers into task-locals + request extensions, so
/// the handler observes the upstream trace id.
#[tokio::test]
async fn tower_layer_inherits_inbound_trace() {
    let svc = tower::service_fn(|req: Request<()>| async move {
        let tp = current_traceparent();
        let ts = current_tracestate();
        let ext = req.extensions().get::<TraceParent>().cloned();
        let state = req.extensions().get::<TraceState>().cloned();
        Ok::<_, Infallible>(Response::new((tp, ts, ext, state)))
    });
    let mut svc = TraceContextLayer::new().layer(svc);

    let req = Request::builder()
        .uri("/x")
        .header(TRACEPARENT_HEADER, traceparent())
        .header(TRACESTATE_HEADER, "congo=t61rcWkgMzE")
        .body(())
        .unwrap();
    let (tp, ts, ext, state) = svc
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap()
        .into_body();

    assert_eq!(tp.as_deref(), Some(traceparent().as_str()));
    assert_eq!(ts.as_deref(), Some("congo=t61rcWkgMzE"));
    let ext = ext.expect("TraceParent in extensions");
    assert_eq!(ext.trace_id, INBOUND_TRACE);
    assert!(ext.sampled());
    assert_eq!(
        state.expect("TraceState in extensions").get("congo"),
        Some("t61rcWkgMzE")
    );
}

/// A malformed traceparent is dropped and the trace is **restarted** — the
/// framework originates a fresh root span (W3C "restart" / OTel root behaviour)
/// rather than propagating the bad header or dropping the trace.
#[tokio::test]
async fn tower_layer_restarts_trace_on_malformed_traceparent() {
    let svc = tower::service_fn(|req: Request<()>| async move {
        Ok::<_, Infallible>(Response::new((
            current_traceparent(),
            req.extensions().get::<TraceParent>().cloned(),
        )))
    });
    let req = Request::builder()
        .uri("/x")
        .header(TRACEPARENT_HEADER, "00-not-a-trace-01")
        .body(())
        .unwrap();
    let (tp, ext) = TraceContextLayer::new()
        .layer(svc)
        .oneshot(req)
        .await
        .unwrap()
        .into_body();
    // The malformed header is NOT propagated; a fresh, valid root is minted.
    let tp = tp.expect("a root span is originated");
    assert!(
        !tp.contains("not-a-trace"),
        "malformed header must be dropped"
    );
    let ext = ext.expect("root TraceParent in extensions");
    assert_eq!(ext.trace_id.len(), 32);
    assert_eq!(ext.parent_id.len(), 16);
    assert!(ext.sampled());
}

/// With no inbound traceparent at all, the layer still originates a root span
/// (so logs + downstream hops get a trace id) — Spring Boot / OTel behaviour.
#[tokio::test]
async fn tower_layer_originates_root_when_absent() {
    let svc = tower::service_fn(|req: Request<()>| async move {
        Ok::<_, Infallible>(Response::new((
            current_traceparent(),
            req.extensions().get::<TraceParent>().cloned(),
        )))
    });
    let req = Request::builder().uri("/x").body(()).unwrap();
    let (tp, ext) = TraceContextLayer::new()
        .layer(svc)
        .oneshot(req)
        .await
        .unwrap()
        .into_body();
    let tp = tp.expect("a root span is originated when no traceparent arrives");
    let ext = ext.expect("root TraceParent in extensions");
    assert_eq!(tp, ext.to_string());
    assert_eq!(ext.trace_id.len(), 32);
    assert!(ext.sampled());
}

/// The reqwest helper stamps the current context onto an outbound request
/// (built offline — no network involved).
#[tokio::test]
async fn reqwest_builder_injection() {
    let client = reqwest::Client::new();
    let request = with_trace_context(Some(traceparent()), None, async {
        inject_reqwest(client.get("http://localhost/down"))
            .build()
            .unwrap()
    })
    .await;
    assert_eq!(
        request
            .headers()
            .get(TRACEPARENT_HEADER)
            .unwrap()
            .to_str()
            .unwrap(),
        traceparent()
    );
    assert!(request.headers().get(TRACESTATE_HEADER).is_none());
}

// ---------------------------------------------------------------------------
// process metrics — pyfly process_metrics gauges
// ---------------------------------------------------------------------------

/// The collector publishes the Micrometer/Spring Boot meter names so
/// Spring dashboards work unchanged.
#[test]
fn process_metrics_use_micrometer_names() {
    let registry = MetricsRegistry::isolated();
    let collector = ProcessMetricsCollector::new();
    collector.collect(&registry);
    let text = registry.prometheus_text();
    assert!(text.contains("process_uptime_seconds"), "{text}");
    assert!(text.contains("process_start_time_seconds"), "{text}");
    assert!(text.contains("system_cpu_count"), "{text}");

    assert!(collector.uptime_seconds() >= 0.0);
    assert!(collector.start_time_seconds() > 0.0);
    assert!(collector.cpu_count() >= 1);
}

// ---------------------------------------------------------------------------
// redaction in the log pipeline — pyfly tests/logging/test_redaction_processor.py
// ---------------------------------------------------------------------------

fn redacting_logger(redaction: RedactionConfig) -> (BufferWriter, impl tracing::Subscriber) {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new().with_redaction(redaction);
    let sub = subscriber_with_writer(cfg, buf.clone());
    (buf, sub)
}

/// pyfly `test_structlog_processor_redacts_event_and_fields`: the message
/// and string fields are scanned; deny-listed keys are replaced wholesale.
#[test]
fn layer_redacts_event_and_fields() {
    let (buf, sub) = redacting_logger(
        RedactionConfig::new()
            .with_entities(["EMAIL"])
            .with_deny_fields(["password"]),
    );
    tracing::subscriber::with_default(sub, || {
        tracing::info!(
            user = "bob@x.io",
            password = "hunter2",
            "login jane@acme.io"
        );
    });

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert_eq!(rec["msg"], "login <EMAIL>");
    assert_eq!(rec["user"], "<EMAIL>");
    assert_eq!(rec["password"], "<REDACTED>");
}

/// pyfly `test_structlog_allow_fields_limits_scanning`: with an allow
/// list, only listed fields (plus the message) are scanned.
#[test]
fn layer_allow_fields_limit_scanning() {
    let (buf, sub) = redacting_logger(
        RedactionConfig::new()
            .with_entities(["EMAIL"])
            .with_allow_fields(["scanned"])
            .with_deny_fields(Vec::<String>::new()),
    );
    tracing::subscriber::with_default(sub, || {
        tracing::info!(
            scanned = "a jane@acme.io",
            note = "keep bob@x.io",
            "msg jane@acme.io"
        );
    });

    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert_eq!(rec["scanned"], "a <EMAIL>");
    assert_eq!(rec["note"], "keep bob@x.io"); // not in allow list -> untouched
    assert_eq!(rec["msg"], "msg <EMAIL>"); // message always scanned
}

/// Partial masking keeps the last 4 characters (pyfly `test_partial_mask`)
/// — wired through the layer.
#[test]
fn layer_partial_mask_in_text_format() {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new()
        .with_format(LogFormat::Text)
        .with_redaction(
            RedactionConfig::new()
                .with_entities(["CREDIT_CARD"])
                .with_mask(MaskStyle::Partial),
        );
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::info!("card 4111 1111 1111 1111 end");
    });
    let out = buf.as_string();
    assert!(out.contains("1111 end"), "{out}");
    assert!(out.contains('*'), "{out}");
    assert!(!out.contains("4111"), "{out}");
}

/// Without a redaction config every wire shape is untouched (backward
/// compatibility with the Go-parity surface).
#[test]
fn no_redaction_config_means_no_redaction() {
    let buf = BufferWriter::new();
    let sub = subscriber_with_writer(LogConfig::new(), buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::info!(password = "hunter2", "mail jane@acme.io");
    });
    let rec: serde_json::Value = serde_json::from_str(buf.as_string().trim()).unwrap();
    assert_eq!(rec["msg"], "mail jane@acme.io");
    assert_eq!(rec["password"], "hunter2");
}

// ---------------------------------------------------------------------------
// per-target levels + runtime LevelHandle — pyfly level map / set_level
// ---------------------------------------------------------------------------

/// The pyfly `{root: INFO, "my.module": DEBUG}` map: a debug record from
/// the configured target passes while others are dropped.
#[test]
fn per_target_levels_gate_by_longest_prefix() {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new()
        .with_level(Level::INFO)
        .with_target_level("noisy::module", Level::DEBUG);
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::debug!(target: "noisy::module::sub", "kept-debug");
        tracing::debug!(target: "other::module", "dropped-debug");
        tracing::info!(target: "other::module", "kept-info");
    });

    let out = buf.as_string();
    assert!(out.contains("kept-debug"), "{out}");
    assert!(!out.contains("dropped-debug"), "{out}");
    assert!(out.contains("kept-info"), "{out}");
}

/// pyfly `LoggingPort.set_level`: levels change at runtime through the
/// handle, without rebuilding the subscriber.
#[test]
fn level_handle_changes_levels_at_runtime() {
    let buf = BufferWriter::new();
    let (sub, handle) = subscriber_with_writer_and_handle(LogConfig::new(), buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::debug!(target: "svc::orders", "before");
        handle.set_level("svc::orders", Level::DEBUG);
        tracing::debug!(target: "svc::orders", "after");
        handle.set_level("root", Level::ERROR);
        tracing::info!(target: "untouched", "info-now-dropped");
    });

    let out = buf.as_string();
    assert!(!out.contains("before"), "{out}");
    assert!(out.contains("after"), "{out}");
    assert!(!out.contains("info-now-dropped"), "{out}");
}

// ---------------------------------------------------------------------------
// file appender through LogConfig — pyfly tests/logging/test_handlers.py
// ---------------------------------------------------------------------------

/// pyfly `build_file_handler` wiring: LogConfig.file tees every record to
/// console AND the rolling file.
#[test]
fn log_config_file_tees_console_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let buf = BufferWriter::new();
    let cfg = LogConfig::new()
        .with_service("orders")
        .with_file(FileConfig::new("app.log").with_path(dir.path().to_string_lossy()));
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::info!("to both sinks");
    });

    let console = buf.as_string();
    let file = std::fs::read_to_string(dir.path().join("app.log")).unwrap();
    assert!(console.contains("to both sinks"), "{console}");
    assert_eq!(
        console, file,
        "console and file must receive the same bytes"
    );
    let rec: serde_json::Value = serde_json::from_str(file.trim()).unwrap();
    assert_eq!(rec["service"], "orders");
}

/// An unopenable file path falls back to console only — a logging
/// misconfiguration must never crash the application (pyfly's
/// audit-grade-robustness rule).
#[test]
fn log_config_file_failure_falls_back_to_console() {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new()
        .with_file(FileConfig::new("app.log").with_path("/dev/null/not-a-directory"));
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::info!("still logged");
    });
    assert!(buf.as_string().contains("still logged"));
}

// ---------------------------------------------------------------------------
// console renderer — pyfly StructlogAdapter ConsoleRenderer branch
// ---------------------------------------------------------------------------

/// `LogFormat::from_name` maps the pyfly format names: `console`/`pretty`/`dev`
/// select the console renderer; `logfmt`/`text` select text; everything else
/// JSON.
#[test]
fn console_format_name_mapping_matches_pyfly() {
    assert_eq!(LogFormat::from_name("console"), LogFormat::Console);
    assert_eq!(LogFormat::from_name("pretty"), LogFormat::Console);
    assert_eq!(LogFormat::from_name("dev"), LogFormat::Console);
    assert_eq!(LogFormat::from_name("logfmt"), LogFormat::Text);
    assert_eq!(LogFormat::from_name("text"), LogFormat::Text);
    assert_eq!(LogFormat::from_name("json"), LogFormat::Json);
    assert_eq!(LogFormat::from_name(""), LogFormat::Json);
}

/// The console renderer emits a human-readable `time [LEVEL] msg key=value`
/// line — leading time/level/msg, trailing fields — and is plain text (no
/// ANSI) by default, matching pyfly's `ConsoleRenderer(colors=False)`.
#[test]
fn console_renderer_emits_pretty_plain_line() {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new().with_format(LogFormat::Console);
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::info!(order_id = "42", "placed order");
    });
    let line = buf.as_string();
    // Not JSON (no leading brace) and not bare logfmt (level is bracketed).
    assert!(!line.trim_start().starts_with('{'), "{line}");
    assert!(line.contains("[INFO"), "{line}");
    assert!(line.contains("placed order"), "{line}");
    assert!(line.contains("order_id=42"), "{line}");
    // Plain by default: no ANSI escape bytes.
    assert!(
        !line.contains('\u{1b}'),
        "default console is uncolored: {line}"
    );
}

/// With `console_colors` enabled, the level is colorized (ANSI escapes
/// present); the message text is still readable.
#[test]
fn console_renderer_colors_when_enabled() {
    let buf = BufferWriter::new();
    let cfg = LogConfig::new()
        .with_format(LogFormat::Console)
        .with_console_colors(true);
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::warn!("watch out");
    });
    let line = buf.as_string();
    assert!(line.contains('\u{1b}'), "colored console has ANSI: {line}");
    assert!(line.contains("watch out"), "{line}");
}

// ---------------------------------------------------------------------------
// external logging-config-file loading — pyfly logging.config_loader
// ---------------------------------------------------------------------------

/// pyfly `test_apply_dictconfig_yaml` analog (JSON dictConfig shape): an
/// external file sets root level, format, and per-target levels, folded over
/// the base config.
#[test]
fn external_json_config_reconfigures_logging() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logging.json");
    std::fs::write(
        &path,
        r#"{"level":"DEBUG","format":"console","levels":{"firefly_web":"WARN"}}"#,
    )
    .unwrap();
    let cfg = load_log_config(&path, LogConfig::default()).unwrap();
    assert_eq!(cfg.level, Level::DEBUG);
    assert_eq!(cfg.format, LogFormat::Console);
    assert_eq!(cfg.levels.get("firefly_web"), Some(&Level::WARN));
}

/// pyfly `test_apply_fileconfig_ini` analog (flat key=value): an external
/// `.properties` file reconfigures logging.
#[test]
fn external_properties_config_reconfigures_logging() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logging.properties");
    std::fs::write(&path, "level = warn\nformat = logfmt\n").unwrap();
    let cfg = load_log_config(&path, LogConfig::default()).unwrap();
    assert_eq!(cfg.level, Level::WARN);
    assert_eq!(cfg.format, LogFormat::Text);
}

/// pyfly `test_apply_missing_returns_false`: a missing path falls back to the
/// base config unchanged and reports `applied == false` (startup not crashed).
#[test]
fn external_config_missing_path_falls_back() {
    let base = LogConfig::default().with_service("svc");
    let (cfg, applied) = apply_external_config("/nope/logging.json", base.clone());
    assert!(!applied);
    assert_eq!(cfg, base);
}

/// An externally-loaded config drives a live subscriber: a `.json` file
/// switching the root level to DEBUG lets debug records through that the
/// default INFO config would have dropped.
#[test]
fn external_config_drives_live_subscriber() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logging.json");
    std::fs::write(&path, r#"{"level":"DEBUG"}"#).unwrap();
    let (cfg, applied) = apply_external_config(&path, LogConfig::default());
    assert!(applied);
    let buf = BufferWriter::new();
    let sub = subscriber_with_writer(cfg, buf.clone());
    tracing::subscriber::with_default(sub, || {
        tracing::debug!("verbose detail");
    });
    assert!(buf.as_string().contains("verbose detail"));
}
