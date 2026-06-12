//! pyfly-parity tests for the firefly-web extensions, ported from
//! `tests/web/test_cors.py`, `test_security_headers.py`,
//! `test_built_in_filters.py`, `test_request_logger.py`,
//! `test_metrics_filter.py`, `test_message_converters.py`,
//! `test_content_negotiation_e2e.py`, and `tests/server/*` — driven
//! through `tower::ServiceExt::oneshot` (no sockets) except the
//! server-module end-to-end cases, which bind port 0.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::routing::{get, post};
use axum::{Extension, Router};
use firefly_kernel::{ProblemDetail, PROBLEM_CONTENT_TYPE, TYPE_FORBIDDEN};
use firefly_web::server::{Server, ServerInfo, ServerProperties, TlsConfig};
use firefly_web::{
    default_message_converters, generate_csrf_token, parse_accept, validate_csrf_token,
    value_to_xml, xml_to_value, ContentNegotiationLayer, CorrelationContext, CorrelationLayer,
    CorsConfig, CorsLayer, CsrfLayer, JsonMessageConverter, MessageConverter, MetricsLayer,
    Negotiate, Outcome, ProblemLayer, RequestLogLayer, RequestMetric, RequestObserver, RollingMax,
    SecurityHeadersConfig, SecurityHeadersLayer, XmlMessageConverter, CSRF_COOKIE_NAME,
    CSRF_HEADER_NAME, PERMIT_DEFAULT_METHODS,
};
use http::{header, HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde::Serialize;
use serde_json::{json, Value};
use tower::ServiceExt;

/// Sends a request through the router and returns status, headers, and
/// collected body bytes.
async fn send(app: Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body.to_vec())
}

fn get_req(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn hello_router() -> Router {
    Router::new().route(
        "/hello",
        get(|| async { axum::Json(json!({"msg": "hello"})) }),
    )
}

// ===================== CORS (pyfly test_cors.py) =====================

#[test]
fn cors_config_defaults() {
    let cfg = CorsConfig::default();
    assert_eq!(cfg.allowed_origins, vec!["*"]);
    assert_eq!(cfg.allowed_methods, vec!["GET"]);
    assert_eq!(cfg.allowed_headers, vec!["*"]);
    assert!(!cfg.allow_credentials);
    assert!(cfg.exposed_headers.is_empty());
    assert_eq!(cfg.max_age, 600);
}

#[test]
fn cors_config_custom() {
    let cfg = CorsConfig {
        allowed_origins: vec!["http://example.com".into()],
        allowed_methods: vec!["GET".into(), "POST".into(), "PUT".into()],
        allowed_headers: vec!["Authorization".into(), "Content-Type".into()],
        allow_credentials: true,
        exposed_headers: vec!["X-Custom-Header".into()],
        max_age: 3600,
    };
    assert_eq!(cfg.allowed_origins, vec!["http://example.com"]);
    assert_eq!(cfg.allowed_methods, vec!["GET", "POST", "PUT"]);
    assert!(cfg.allow_credentials);
    assert_eq!(cfg.exposed_headers, vec!["X-Custom-Header"]);
    assert_eq!(cfg.max_age, 3600);
}

#[test]
fn cors_permit_defaults_is_spring_method_set() {
    let cfg = CorsConfig::permit_defaults();
    assert_eq!(cfg.allowed_methods, PERMIT_DEFAULT_METHODS.to_vec());
}

#[test]
fn cors_config_deserializes_kebab_case() {
    let cfg: CorsConfig = serde_json::from_value(json!({
        "allowed-origins": ["http://example.com"],
        "allow-credentials": true,
        "max-age": 60,
    }))
    .unwrap();
    assert_eq!(cfg.allowed_origins, vec!["http://example.com"]);
    assert!(cfg.allow_credentials);
    assert_eq!(cfg.max_age, 60);
}

#[tokio::test]
async fn cors_preflight_request() {
    let app = hello_router().layer(CorsLayer::new(CorsConfig {
        allowed_origins: vec!["http://example.com".into()],
        allowed_methods: vec!["GET".into(), "POST".into()],
        ..CorsConfig::default()
    }));
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/hello")
        .header(header::ORIGIN, "http://example.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "http://example.com"
    );
    let methods = headers
        .get(header::ACCESS_CONTROL_ALLOW_METHODS)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(methods.contains("POST"));
    assert_eq!(headers.get(header::ACCESS_CONTROL_MAX_AGE).unwrap(), "600");
}

#[tokio::test]
async fn cors_preflight_disallowed_origin_is_400() {
    let app = hello_router().layer(CorsLayer::new(CorsConfig {
        allowed_origins: vec!["http://example.com".into()],
        ..CorsConfig::default()
    }));
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/hello")
        .header(header::ORIGIN, "http://evil.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    assert_eq!(String::from_utf8(body).unwrap(), "Disallowed CORS origin");
}

#[tokio::test]
async fn cors_preflight_echoes_requested_headers_for_wildcard() {
    let app = hello_router().layer(CorsLayer::new(CorsConfig {
        allowed_origins: vec!["http://example.com".into()],
        allowed_methods: vec!["GET".into(), "POST".into()],
        ..CorsConfig::default()
    }));
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/hello")
        .header(header::ORIGIN, "http://example.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "x-custom, x-other")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_HEADERS).unwrap(),
        "x-custom, x-other"
    );
}

#[tokio::test]
async fn cors_simple_request() {
    let app = hello_router().layer(CorsLayer::new(CorsConfig {
        allowed_origins: vec!["http://example.com".into()],
        ..CorsConfig::default()
    }));
    let req = Request::builder()
        .uri("/hello")
        .header(header::ORIGIN, "http://example.com")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(app, req).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed, json!({"msg": "hello"}));
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "http://example.com"
    );
}

#[tokio::test]
async fn cors_wildcard_origin_without_credentials_reflects_star() {
    let app = hello_router().layer(CorsLayer::default());
    let req = Request::builder()
        .uri("/hello")
        .header(header::ORIGIN, "http://anywhere.test")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(app, req).await;
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "*"
    );
}

#[tokio::test]
async fn cors_wildcard_with_credentials_echoes_origin() {
    let app = hello_router().layer(CorsLayer::new(CorsConfig {
        allow_credentials: true,
        ..CorsConfig::default()
    }));
    let req = Request::builder()
        .uri("/hello")
        .header(header::ORIGIN, "http://app.test")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(app, req).await;
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "http://app.test"
    );
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .unwrap(),
        "true"
    );
}

#[tokio::test]
async fn cors_exposed_headers_on_simple_response() {
    let app = hello_router().layer(CorsLayer::new(CorsConfig {
        allowed_origins: vec!["http://example.com".into()],
        exposed_headers: vec!["X-Custom-Header".into()],
        ..CorsConfig::default()
    }));
    let req = Request::builder()
        .uri("/hello")
        .header(header::ORIGIN, "http://example.com")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(app, req).await;
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_EXPOSE_HEADERS).unwrap(),
        "X-Custom-Header"
    );
}

#[tokio::test]
async fn no_cors_when_not_configured() {
    let app = hello_router();
    let req = Request::builder()
        .uri("/hello")
        .header(header::ORIGIN, "http://example.com")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

// ========= Security headers (pyfly test_security_headers.py) =========

#[tokio::test]
async fn security_headers_defaults_applied() {
    let app = hello_router().layer(SecurityHeadersLayer::new(SecurityHeadersConfig::default()));
    let (status, headers, _) = send(app, get_req("/hello")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        headers.get("strict-transport-security").unwrap(),
        "max-age=31536000; includeSubDomains"
    );
    assert_eq!(headers.get("x-xss-protection").unwrap(), "0");
    assert_eq!(
        headers.get("referrer-policy").unwrap(),
        "strict-origin-when-cross-origin"
    );
}

#[tokio::test]
async fn security_headers_custom_config() {
    let config = SecurityHeadersConfig {
        x_frame_options: "SAMEORIGIN".into(),
        strict_transport_security: "max-age=86400".into(),
        x_xss_protection: "1; mode=block".into(),
        referrer_policy: "no-referrer".into(),
        ..SecurityHeadersConfig::default()
    };
    let app = hello_router().layer(SecurityHeadersLayer::new(config));
    let (_, headers, _) = send(app, get_req("/hello")).await;
    assert_eq!(headers.get("x-frame-options").unwrap(), "SAMEORIGIN");
    assert_eq!(
        headers.get("strict-transport-security").unwrap(),
        "max-age=86400"
    );
    assert_eq!(headers.get("x-xss-protection").unwrap(), "1; mode=block");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
}

#[tokio::test]
async fn security_headers_csp_when_configured_absent_when_none() {
    let with_csp = SecurityHeadersConfig {
        content_security_policy: Some("default-src 'self'".into()),
        ..SecurityHeadersConfig::default()
    };
    let app = hello_router().layer(SecurityHeadersLayer::new(with_csp));
    let (_, headers, _) = send(app, get_req("/hello")).await;
    assert_eq!(
        headers.get("content-security-policy").unwrap(),
        "default-src 'self'"
    );

    let app = hello_router().layer(SecurityHeadersLayer::new(SecurityHeadersConfig::default()));
    let (_, headers, _) = send(app, get_req("/hello")).await;
    assert!(headers.get("content-security-policy").is_none());
    assert!(headers.get("permissions-policy").is_none());
}

#[tokio::test]
async fn security_headers_permissions_policy_when_configured() {
    let config = SecurityHeadersConfig {
        permissions_policy: Some("camera=()".into()),
        ..SecurityHeadersConfig::default()
    };
    let app = hello_router().layer(SecurityHeadersLayer::new(config));
    let (_, headers, _) = send(app, get_req("/hello")).await;
    assert_eq!(headers.get("permissions-policy").unwrap(), "camera=()");
}

// ================ CSRF (pyfly csrf_filter contract) ==================

fn csrf_router() -> Router {
    Router::new()
        .route("/submit", post(|| async { "submitted" }))
        .route("/page", get(|| async { "page" }))
        .route("/actuator/health", post(|| async { "up" }))
        .layer(CsrfLayer::new())
}

fn csrf_cookie_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(CSRF_COOKIE_NAME))
        .map(ToOwned::to_owned)
}

#[test]
fn csrf_token_generation_and_validation() {
    let a = generate_csrf_token();
    let b = generate_csrf_token();
    // Python's secrets.token_urlsafe(32) — 43 URL-safe chars.
    assert_eq!(a.len(), 43);
    assert_ne!(a, b);
    assert!(validate_csrf_token(&a, &a));
    assert!(!validate_csrf_token(&a, &b));
}

#[tokio::test]
async fn csrf_safe_method_sets_cookie() {
    let (status, headers, _) = send(csrf_router(), get_req("/page")).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = csrf_cookie_from(&headers).expect("XSRF-TOKEN cookie set");
    assert!(cookie.contains("Path=/"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Secure"));
    assert!(!cookie.contains("HttpOnly"), "JS must read the token");
    let token = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches(&format!("{CSRF_COOKIE_NAME}="))
        .to_string();
    assert_eq!(token.len(), 43);
}

#[tokio::test]
async fn csrf_unsafe_without_tokens_is_403_problem() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/submit")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(csrf_router(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        PROBLEM_CONTENT_TYPE
    );
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_FORBIDDEN);
    assert_eq!(pd.detail, "CSRF token missing");
}

#[tokio::test]
async fn csrf_mismatched_tokens_is_403_invalid() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/submit")
        .header(header::COOKIE, format!("{CSRF_COOKIE_NAME}=token-a"))
        .header(CSRF_HEADER_NAME, "token-b")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(csrf_router(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.detail, "CSRF token invalid");
}

#[tokio::test]
async fn csrf_matching_tokens_pass_and_rotate() {
    let token = generate_csrf_token();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/submit")
        .header(header::COOKIE, format!("{CSRF_COOKIE_NAME}={token}"))
        .header(CSRF_HEADER_NAME, &token)
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(csrf_router(), req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(String::from_utf8(body).unwrap(), "submitted");
    // Token rotated: a fresh cookie that differs from the submitted one.
    let cookie = csrf_cookie_from(&headers).expect("rotated cookie");
    assert!(!cookie.contains(&token));
}

#[tokio::test]
async fn csrf_bearer_bypass() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/submit")
        .header(header::AUTHORIZATION, "Bearer some.jwt.token")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(csrf_router(), req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(csrf_cookie_from(&headers).is_none());
}

#[tokio::test]
async fn csrf_excluded_paths_bypass() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/actuator/health")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(csrf_router(), req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(csrf_cookie_from(&headers).is_none());
}

// ===== Correlation extension (pyfly correlation/transaction filters) =====

#[tokio::test]
async fn correlation_mints_request_and_transaction_ids() {
    let app = Router::new()
        .route("/x", get(|| async { StatusCode::OK }))
        .layer(CorrelationLayer::new());
    let (status, headers, _) = send(app, get_req("/x")).await;
    assert_eq!(status, StatusCode::OK);
    // UUID v4 format (36 chars), exactly like pyfly's str(uuid.uuid4()).
    assert_eq!(
        headers.get("X-Request-Id").unwrap().to_str().unwrap().len(),
        36
    );
    assert_eq!(
        headers
            .get("X-Transaction-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .len(),
        36
    );
    // Tenant is never generated server-side.
    assert!(headers.get("X-Tenant-Id").is_none());
    assert!(headers.get("traceparent").is_none());
}

#[tokio::test]
async fn correlation_propagates_full_surface() {
    let app = Router::new()
        .route("/x", get(|| async { StatusCode::OK }))
        .layer(CorrelationLayer::new());
    let req = Request::builder()
        .uri("/x")
        .header("X-Correlation-Id", "corr-1")
        .header("X-Request-Id", "req-1")
        .header("X-Tenant-Id", "tenant-1")
        .header("X-Transaction-Id", "custom-123")
        .header("traceparent", "00-abc-def-01")
        .header("tracestate", "vendor=1")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(app, req).await;
    assert_eq!(headers.get("X-Correlation-Id").unwrap(), "corr-1");
    assert_eq!(headers.get("X-Request-Id").unwrap(), "req-1");
    assert_eq!(headers.get("X-Tenant-Id").unwrap(), "tenant-1");
    assert_eq!(headers.get("X-Transaction-Id").unwrap(), "custom-123");
    assert_eq!(headers.get("traceparent").unwrap(), "00-abc-def-01");
    assert_eq!(headers.get("tracestate").unwrap(), "vendor=1");
}

#[tokio::test]
async fn correlation_context_in_extensions_and_task_local() {
    let captured = Arc::new(Mutex::new(None::<CorrelationContext>));
    let captured_clone = Arc::clone(&captured);
    let app = Router::new()
        .route(
            "/x",
            get(move |Extension(ctx): Extension<CorrelationContext>| {
                let captured = Arc::clone(&captured_clone);
                async move {
                    // Extension and task-local views agree.
                    let ambient = firefly_web::current_correlation_context().unwrap();
                    assert_eq!(ambient, ctx);
                    *captured.lock().unwrap() = Some(ctx);
                    StatusCode::OK
                }
            }),
        )
        .layer(CorrelationLayer::new());
    let req = Request::builder()
        .uri("/x")
        .header("X-Tenant-Id", "acme")
        .header("X-Transaction-Id", "tx-9")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(app, req).await;
    assert_eq!(status, StatusCode::OK);
    let ctx = captured.lock().unwrap().clone().unwrap();
    assert_eq!(ctx.tenant_id.as_deref(), Some("acme"));
    assert_eq!(ctx.transaction_id, "tx-9");
    assert!(!ctx.correlation_id.is_empty());
    assert!(!ctx.request_id.is_empty());
    assert!(ctx.traceparent.is_none());
}

#[test]
fn correlation_context_absent_outside_scope() {
    assert!(firefly_web::current_correlation_context().is_none());
}

// ========== Request log (pyfly test_request_logger.py) ==============

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[tokio::test]
async fn request_log_passes_through_and_emits_event() {
    use tracing::instrument::WithSubscriber;

    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();

    let app = Router::new()
        .route("/items", get(|| async { ([("x-custom", "val")], "hello") }))
        .layer(RequestLogLayer::new());
    let fut = async {
        let (status, headers, body) = send(app, get_req("/items")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("x-custom").unwrap(), "val");
        assert_eq!(String::from_utf8(body).unwrap(), "hello");
    };
    fut.with_subscriber(subscriber).await;

    let logged = buffer.contents();
    assert!(logged.contains("http_request"), "got: {logged}");
    assert!(logged.contains("method=GET"));
    assert!(logged.contains("path=/items"));
    assert!(logged.contains("status_code=200"));
    assert!(logged.contains("duration_ms="));
}

#[tokio::test]
async fn request_log_error_response_still_logged() {
    use tracing::instrument::WithSubscriber;

    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();

    let app = Router::new()
        .route(
            "/fail",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "error") }),
        )
        .layer(RequestLogLayer::new());
    let fut = async {
        let (status, _, _) = send(app, get_req("/fail")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    };
    fut.with_subscriber(subscriber).await;

    let logged = buffer.contents();
    assert!(logged.contains("status_code=500"), "got: {logged}");
}

#[tokio::test]
async fn request_log_panic_logged_as_failed_and_recovered() {
    use tracing::instrument::WithSubscriber;

    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();

    let app = Router::new()
        .route(
            "/boom",
            get(|| async {
                panic!("kaboom");
                #[allow(unreachable_code)]
                ""
            }),
        )
        .layer(RequestLogLayer::new())
        .layer(ProblemLayer::new());
    let fut = async {
        let (status, _, _) = send(app, get_req("/boom")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    };
    fut.with_subscriber(subscriber).await;

    let logged = buffer.contents();
    assert!(logged.contains("http_request_failed"), "got: {logged}");
    assert!(logged.contains("kaboom"));
}

// ============ Metrics (pyfly test_metrics_filter.py) =================

#[derive(Default)]
struct RecordingObserver(Mutex<Vec<RequestMetric>>);

impl RequestObserver for RecordingObserver {
    fn record(&self, metric: &RequestMetric) {
        self.0.lock().unwrap().push(metric.clone());
    }
}

impl RecordingObserver {
    fn metrics(&self) -> Vec<RequestMetric> {
        self.0.lock().unwrap().clone()
    }
}

fn metrics_app(observer: Arc<RecordingObserver>) -> Router {
    Router::new()
        .route(
            "/users/:user_id",
            get(
                |axum::extract::Path(user_id): axum::extract::Path<String>| async move {
                    axum::Json(json!({"id": user_id}))
                },
            ),
        )
        .route(
            "/boom",
            get(|| async {
                panic!("kaboom");
                #[allow(unreachable_code)]
                ""
            }),
        )
        .layer(MetricsLayer::new(observer))
        .layer(ProblemLayer::new())
}

#[tokio::test]
async fn metrics_uri_tag_is_templated_not_raw_path() {
    let observer = Arc::new(RecordingObserver::default());
    let app = metrics_app(Arc::clone(&observer));
    send(app.clone(), get_req("/users/42")).await;
    send(app, get_req("/users/99")).await;

    let metrics = observer.metrics();
    assert_eq!(metrics.len(), 2);
    // Both requests collapse onto the single templated series, in
    // Micrometer spelling — the raw path never appears.
    for m in &metrics {
        assert_eq!(m.uri, "/users/{user_id}");
        assert_eq!(m.method, "GET");
        assert_eq!(m.status, 200);
        assert_eq!(m.outcome, Outcome::Success);
        assert_eq!(m.exception, None);
        assert!(m.duration_seconds >= 0.0);
    }
}

#[tokio::test]
async fn metrics_not_found_uri_and_client_error_outcome() {
    let observer = Arc::new(RecordingObserver::default());
    let app = metrics_app(Arc::clone(&observer));
    send(app, get_req("/nope")).await;

    let metrics = observer.metrics();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].uri, "NOT_FOUND");
    assert_eq!(metrics[0].status, 404);
    assert_eq!(metrics[0].outcome, Outcome::ClientError);
}

#[tokio::test]
async fn metrics_panic_records_server_error_with_exception() {
    let observer = Arc::new(RecordingObserver::default());
    let app = metrics_app(Arc::clone(&observer));
    let (status, _, _) = send(app, get_req("/boom")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let metrics = observer.metrics();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].uri, "/boom");
    assert_eq!(metrics[0].status, 500);
    assert_eq!(metrics[0].outcome, Outcome::ServerError);
    assert_eq!(metrics[0].exception.as_deref(), Some("panic"));
}

#[tokio::test]
async fn metrics_rolling_max_is_populated() {
    let observer = Arc::new(RecordingObserver::default());
    let app = metrics_app(Arc::clone(&observer));
    send(app, get_req("/users/1")).await;

    let metrics = observer.metrics();
    assert_eq!(metrics.len(), 1);
    assert!(metrics[0].rolling_max_seconds > 0.0);
    assert!(metrics[0].rolling_max_seconds >= metrics[0].duration_seconds);
}

#[tokio::test]
async fn metrics_exclusions() {
    let observer = Arc::new(RecordingObserver::default());
    let app = Router::new()
        .route("/actuator/prometheus", get(|| async { "scrape" }))
        .route("/admin/api/sse/metrics", get(|| async { "sse" }))
        .route("/api/users", get(|| async { "users" }))
        .layer(MetricsLayer::new(
            Arc::clone(&observer) as Arc<dyn RequestObserver>
        ));
    send(app.clone(), get_req("/actuator/prometheus")).await;
    send(app.clone(), get_req("/admin/api/sse/metrics")).await;
    send(app, get_req("/api/users")).await;

    let metrics = observer.metrics();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].uri, "/api/users");
}

#[test]
fn metrics_outcome_for_status() {
    let cases = [
        (100, Outcome::Informational),
        (200, Outcome::Success),
        (204, Outcome::Success),
        (301, Outcome::Redirection),
        (404, Outcome::ClientError),
        (422, Outcome::ClientError),
        (500, Outcome::ServerError),
        (503, Outcome::ServerError),
        (700, Outcome::Unknown),
    ];
    for (status, expected) in cases {
        assert_eq!(Outcome::from_status(status), expected, "status {status}");
    }
    assert_eq!(Outcome::Success.as_str(), "SUCCESS");
    assert_eq!(Outcome::ClientError.as_str(), "CLIENT_ERROR");
    assert_eq!(Outcome::Informational.as_str(), "INFORMATIONAL");
}

#[test]
fn rolling_max_two_window_decay() {
    let mut rm = RollingMax::new(60.0);
    // First window: max is the largest sample.
    assert_eq!(rm.record(5.0, 0.0), 5.0);
    assert_eq!(rm.record(2.0, 10.0), 5.0);
    // Next window: previous window's max is carried.
    assert_eq!(rm.record(1.0, 61.0), 5.0);
    // Two windows later the 5.0 has decayed out; 1.0 carries.
    assert_eq!(rm.record(0.5, 121.0), 1.0);
    // A jump of more than one window expires everything.
    assert_eq!(rm.record(0.2, 600.0), 0.2);
}

// ==== Content negotiation (pyfly test_message_converters.py + e2e) ====

#[test]
fn parse_accept_orders_by_qvalue() {
    assert_eq!(
        parse_accept(Some("application/json;q=0.8, application/xml;q=0.9")),
        vec!["application/xml", "application/json"]
    );
    assert_eq!(parse_accept(None), vec!["application/json"]);
    // Equal q preserves header order.
    assert_eq!(
        parse_accept(Some("application/xml, application/json")),
        vec!["application/xml", "application/json"]
    );
}

#[test]
fn json_converter_roundtrip() {
    let c = JsonMessageConverter::new();
    let value = json!({"name": "a", "qty": 2});
    let (body, content_type) = c.write(&value).unwrap();
    assert_eq!(content_type, "application/json");
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), value);
    assert_eq!(c.read(&body).unwrap(), value);
    // pyfly decodes an empty body as JSON null.
    assert_eq!(c.read(b"").unwrap(), Value::Null);
}

#[test]
fn xml_converter_roundtrip() {
    let c = XmlMessageConverter::new();
    let value = json!({"name": "a", "qty": 2});
    let (body, content_type) = c.write(&value).unwrap();
    assert_eq!(content_type, "application/xml");
    let text = String::from_utf8(body.clone()).unwrap();
    assert!(text.contains("<name>a</name>"), "got: {text}");
    assert!(text.contains("<qty>2</qty>"), "got: {text}");
    // XML is untyped: scalar leaves read back as strings (pydantic
    // coerces on the Python side; serde callers coerce here).
    assert_eq!(c.read(&body).unwrap(), json!({"name": "a", "qty": "2"}));
}

#[test]
fn xml_value_mapping_lists_nulls_and_nesting() {
    let xml = value_to_xml(&json!({"items": [1, 2], "none": null}), "response").unwrap();
    assert!(
        xml.contains("<items>1</items><items>2</items>"),
        "got: {xml}"
    );
    assert!(xml.contains("<none/>"), "got: {xml}");

    let parsed = xml_to_value("<root><a>1</a><a>2</a><b><c>x</c></b><d/></root>").unwrap();
    assert_eq!(
        parsed,
        json!({"root": {"a": ["1", "2"], "b": {"c": "x"}, "d": null}})
    );
}

#[test]
fn registry_negotiation() {
    let reg = default_message_converters();
    let xml_value = json!({"k": "v"});
    let (_, ct) = reg
        .find_writer(Some("application/xml"))
        .unwrap()
        .write(&xml_value)
        .unwrap();
    assert_eq!(ct, "application/xml");
    let (_, ct) = reg
        .find_writer(Some("application/json"))
        .unwrap()
        .write(&xml_value)
        .unwrap();
    assert_eq!(ct, "application/json");
    let (_, ct) = reg.find_writer(None).unwrap().write(&xml_value).unwrap();
    assert_eq!(ct, "application/json"); // JSON is the default
    let (_, ct) = reg
        .find_writer(Some("application/xml;q=0.9, application/json;q=0.8"))
        .unwrap()
        .write(&xml_value)
        .unwrap();
    assert_eq!(ct, "application/xml");

    assert!(reg
        .find_reader(Some("application/xml"))
        .unwrap()
        .supports("text/xml"));
    assert!(reg
        .find_reader(Some("application/json"))
        .unwrap()
        .supports("application/json"));
    // No content type falls back to the first converter (JSON).
    assert!(reg.find_reader(None).unwrap().supports("application/json"));
}

#[test]
fn registry_user_converter_takes_priority() {
    struct CborConverter;
    impl MessageConverter for CborConverter {
        fn media_types(&self) -> &[&str] {
            &["application/cbor"]
        }
        fn read(&self, _body: &[u8]) -> Result<Value, firefly_kernel::FireflyError> {
            Ok(Value::Null)
        }
        fn write(&self, _value: &Value) -> Result<(Vec<u8>, String), firefly_kernel::FireflyError> {
            Ok((vec![0x00], "application/cbor".to_string()))
        }
    }

    let mut reg = default_message_converters();
    reg.add(Arc::new(CborConverter));
    let (_, ct) = reg
        .find_writer(Some("application/cbor"))
        .unwrap()
        .write(&Value::Null)
        .unwrap();
    assert_eq!(ct, "application/cbor");
    // Others still work.
    let (_, ct) = reg
        .find_writer(Some("application/json"))
        .unwrap()
        .write(&Value::Null)
        .unwrap();
    assert_eq!(ct, "application/json");
}

#[derive(Serialize, Clone)]
struct Widget {
    name: String,
    qty: u32,
}

fn widget_router() -> Router {
    Router::new()
        .route(
            "/widget",
            get(|| async {
                Negotiate(Widget {
                    name: "gadget".into(),
                    qty: 3,
                })
            }),
        )
        .layer(ContentNegotiationLayer::default())
}

#[tokio::test]
async fn e2e_response_negotiates_json_and_xml() {
    let req = Request::builder()
        .uri("/widget")
        .header(header::ACCEPT, "application/json")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(widget_router(), req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"name": "gadget", "qty": 3})
    );

    let req = Request::builder()
        .uri("/widget")
        .header(header::ACCEPT, "application/xml")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(widget_router(), req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/xml"));
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("<name>gadget</name>"), "got: {text}");
    assert!(text.contains("<qty>3</qty>"), "got: {text}");
}

#[tokio::test]
async fn e2e_qvalues_prefer_xml() {
    let req = Request::builder()
        .uri("/widget")
        .header(
            header::ACCEPT,
            "application/json;q=0.5, application/xml;q=0.9",
        )
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(widget_router(), req).await;
    assert!(headers
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/xml"));
}

#[tokio::test]
async fn negotiate_without_layer_defaults_to_json() {
    let app = Router::new().route(
        "/widget",
        get(|| async {
            Negotiate(Widget {
                name: "gadget".into(),
                qty: 3,
            })
        }),
    );
    let req = Request::builder()
        .uri("/widget")
        .header(header::ACCEPT, "application/xml")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"name": "gadget", "qty": 3})
    );
}

// =================== Server module (server brief) ====================

#[test]
fn server_properties_defaults() {
    let props: ServerProperties = serde_json::from_value(json!({})).unwrap();
    assert_eq!(props, ServerProperties::default());
    assert_eq!(props.host, "0.0.0.0");
    assert_eq!(props.port, 8000);
    assert_eq!(props.backlog, 1024);
    assert_eq!(props.graceful_timeout, 30);
    assert_eq!(props.keep_alive_timeout, 5);
    assert_eq!(props.max_concurrent_connections, None);
    assert!(props.tls.is_none());
}

#[test]
fn server_properties_deserializes_with_tls_and_aliases() {
    let props: ServerProperties = serde_json::from_value(json!({
        "host": "127.0.0.1",
        "port": 8443,
        "graceful-timeout": 5,
        "keep-alive-timeout": 10,
        "max-concurrent-connections": 64,
        "tls": {"ssl_certfile": "/tmp/cert.pem", "ssl_keyfile": "/tmp/key.pem"},
    }))
    .unwrap();
    assert_eq!(props.host, "127.0.0.1");
    assert_eq!(props.port, 8443);
    assert_eq!(props.graceful_timeout, 5);
    assert_eq!(props.keep_alive_timeout, 10);
    assert_eq!(props.max_concurrent_connections, Some(64));
    assert_eq!(
        props.tls,
        Some(TlsConfig {
            cert_file: "/tmp/cert.pem".into(),
            key_file: "/tmp/key.pem".into(),
        })
    );
}

#[test]
fn server_info_snapshot() {
    let props = ServerProperties {
        host: "127.0.0.1".into(),
        port: 9000,
        tls: Some(TlsConfig {
            cert_file: "c.pem".into(),
            key_file: "k.pem".into(),
        }),
        ..ServerProperties::default()
    };
    let info = ServerInfo::from_properties(&props);
    assert_eq!(info.name, "hyper");
    assert_eq!(info.version, firefly_web::VERSION);
    assert_eq!(info.host, "127.0.0.1");
    assert_eq!(info.port, 9000);
    assert_eq!(info.http_protocol, "auto");
    assert!(info.tls);
    // Serializes for the /actuator/info contributor.
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["name"], "hyper");
    assert_eq!(json["tls"], true);
}

fn ephemeral_props() -> ServerProperties {
    ServerProperties {
        host: "127.0.0.1".into(),
        port: 0,
        graceful_timeout: 1,
        ..ServerProperties::default()
    }
}

#[test]
fn server_bind_resolves_port_zero() {
    let server = Server::bind(&ephemeral_props()).unwrap();
    let addr = server.local_addr();
    assert_ne!(addr.port(), 0);
    let info = server.info();
    assert_eq!(info.port, addr.port());
    assert!(!info.tls);
}

/// Raw HTTP/1.1 GET over a plain TCP stream (no client dependency).
async fn raw_http_get(addr: std::net::SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

#[tokio::test]
async fn serve_plain_http_end_to_end_with_graceful_shutdown() {
    let server = Server::bind(&ephemeral_props()).unwrap();
    let addr = server.local_addr();
    let router = Router::new().route("/", get(|| async { "ok" }));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.serve(router, async move {
        let _ = rx.await;
    }));

    let response = raw_http_get(addr, "/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    assert!(response.ends_with("ok"), "got: {response}");

    tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("server drained within budget")
        .unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn serve_with_concurrency_limit_still_serves() {
    let props = ServerProperties {
        max_concurrent_connections: Some(2),
        ..ephemeral_props()
    };
    let server = Server::bind(&props).unwrap();
    let addr = server.local_addr();
    let router = Router::new().route("/", get(|| async { "limited" }));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.serve(router, async move {
        let _ = rx.await;
    }));

    let response = raw_http_get(addr, "/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    assert!(response.ends_with("limited"), "got: {response}");

    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test]
async fn serve_tls_starts_and_shuts_down() {
    let props = ServerProperties {
        tls: Some(TlsConfig {
            cert_file: fixture("cert.pem"),
            key_file: fixture("key.pem"),
        }),
        ..ephemeral_props()
    };
    let server = Server::bind(&props).unwrap();
    assert!(server.info().tls);
    let router = Router::new().route("/", get(|| async { "secure" }));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.serve(router, async move {
        let _ = rx.await;
    }));
    // Give the TLS config a beat to load, then shut down: reaching a
    // clean exit proves the PEM fixture parsed and the acceptor ran.
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("tls server drained")
        .unwrap();
    assert!(result.is_ok(), "tls serve failed: {result:?}");
}

#[tokio::test]
async fn serve_tls_missing_files_errors() {
    let props = ServerProperties {
        tls: Some(TlsConfig {
            cert_file: "/nonexistent/cert.pem".into(),
            key_file: "/nonexistent/key.pem".into(),
        }),
        ..ephemeral_props()
    };
    let server = Server::bind(&props).unwrap();
    let router = Router::new();
    let result = server.serve(router, async {}).await;
    assert!(result.is_err());
}

// ============ misc: token uniqueness sanity (csrf helper) ============

#[test]
fn csrf_tokens_are_unique() {
    let tokens: HashSet<String> = (0..64).map(|_| generate_csrf_token()).collect();
    assert_eq!(tokens.len(), 64);
}
