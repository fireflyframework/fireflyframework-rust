//! Integration tests for the reactive [`WebClient`] surface.
//!
//! Each test spawns a real axum server on a random localhost port — the
//! `httptest.NewServer` analog used by the eager-client suite — and
//! asserts the reactive `Mono` / `Flux` terminals against it: NDJSON and
//! SSE streaming via `body_to_flux`, `Mono` GET/POST round-trips,
//! RFC 7807 problem decode, correlation propagation, and the raw
//! `exchange` response. No test sleeps; the slowest streaming case stays
//! far under the 200 ms budget.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Json;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use futures::stream;
use http::Method;
use serde::{Deserialize, Serialize};

use firefly_client::{WebClientBuilder, NDJSON_CONTENT_TYPE, SSE_CONTENT_TYPE};
use firefly_kernel::{with_correlation_id, ProblemDetail, PROBLEM_CONTENT_TYPE, TYPE_NOT_FOUND};

/// Binds an axum router on a random localhost port and returns the base
/// URL — the `httptest.NewServer` analog.
async fn spawn_server(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
struct Tick {
    seq: u64,
}

#[derive(Serialize, Deserialize)]
struct CreateUser {
    name: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
struct User {
    id: String,
    name: String,
}

// --- body_to_mono round-trips ---------------------------------------------

#[tokio::test]
async fn mono_get_round_trip() {
    let app = Router::new().route(
        "/orders/1",
        get(|| async {
            Json(User {
                id: "u1".into(),
                name: "alice".into(),
            })
        }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    let out = client
        .method(Method::GET)
        .uri("/orders/1")
        .retrieve()
        .body_to_mono::<User>()
        .block()
        .await
        .expect("mono ok")
        .expect("present");
    assert_eq!(out.id, "u1");
    assert_eq!(out.name, "alice");
}

#[tokio::test]
async fn mono_post_round_trip_with_body() {
    let app = Router::new().route(
        "/users",
        post(|Json(input): Json<CreateUser>| async move {
            (
                StatusCode::CREATED,
                Json(User {
                    id: "u9".into(),
                    name: input.name,
                }),
            )
        }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    let out = client
        .post()
        .uri("/users")
        .body(&CreateUser { name: "bob".into() })
        .retrieve()
        .body_to_mono::<User>()
        .block()
        .await
        .expect("mono ok")
        .expect("present");
    assert_eq!(
        out,
        User {
            id: "u9".into(),
            name: "bob".into(),
        }
    );
}

#[tokio::test]
async fn mono_empty_body_decodes_to_unit() {
    let app = Router::new().route("/x", get(|| async { StatusCode::NO_CONTENT }));
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    // `()` over a 204 body must decode (empty -> JSON null -> ()).
    client
        .get()
        .uri("/x")
        .retrieve()
        .body_to_mono::<()>()
        .block()
        .await
        .expect("unit decode of empty body");
}

#[tokio::test]
async fn query_params_reach_the_server() {
    let app = Router::new().route(
        "/search",
        // Echo the raw query string back as a JSON string body.
        get(
            |axum::extract::RawQuery(q): axum::extract::RawQuery| async move {
                Json(q.unwrap_or_default())
            },
        ),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    let echoed: String = client
        .get()
        .uri("/search")
        .query("page", "2")
        .query("size", "10")
        .retrieve()
        .body_to_mono::<String>()
        .block()
        .await
        .expect("mono ok")
        .expect("present");
    assert!(echoed.contains("page=2"), "got {echoed}");
    assert!(echoed.contains("size=10"), "got {echoed}");
}

// --- body_to_flux: NDJSON streaming ---------------------------------------

/// Emits an `application/x-ndjson` body of `n` ticks, one JSON document
/// per newline-terminated line — exactly what `firefly-web`'s `Flux`
/// responder produces.
fn ndjson_route(n: u64) -> Router {
    Router::new().route(
        "/ticks",
        get(move || async move {
            let lines = stream::iter((0..n).map(|seq| {
                let doc = serde_json::to_string(&Tick { seq }).expect("encode");
                Ok::<_, Infallible>(format!("{doc}\n"))
            }));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, NDJSON_CONTENT_TYPE)
                .body(Body::from_stream(lines))
                .expect("response")
        }),
    )
}

#[tokio::test]
async fn flux_streams_ndjson_elements() {
    let base = spawn_server(ndjson_route(5)).await;
    let client = WebClientBuilder::new(&base).build();

    let ticks = client
        .get()
        .uri("/ticks")
        .retrieve()
        .body_to_flux::<Tick>()
        .collect_list()
        .block()
        .await
        .expect("flux ok")
        .expect("list");
    assert_eq!(ticks, (0..5).map(|seq| Tick { seq }).collect::<Vec<_>>());
}

#[tokio::test]
async fn flux_ndjson_operators_compose() {
    let base = spawn_server(ndjson_route(10)).await;
    let client = WebClientBuilder::new(&base).build();

    // Lazily filter + map the streamed elements, Reactor-style.
    let evens = client
        .get()
        .uri("/ticks")
        .retrieve()
        .body_to_flux::<Tick>()
        .filter(|t| t.seq % 2 == 0)
        .map(|t| t.seq)
        .collect_list()
        .block()
        .await
        .expect("flux ok")
        .expect("list");
    assert_eq!(evens, vec![0, 2, 4, 6, 8]);
}

#[tokio::test]
async fn flux_empty_ndjson_yields_empty_list() {
    let base = spawn_server(ndjson_route(0)).await;
    let client = WebClientBuilder::new(&base).build();

    let ticks = client
        .get()
        .uri("/ticks")
        .retrieve()
        .body_to_flux::<Tick>()
        .collect_list()
        .block()
        .await
        .expect("flux ok")
        .expect("list");
    assert!(ticks.is_empty());
}

// --- body_to_flux: SSE streaming ------------------------------------------

/// Emits a `text/event-stream` body of `n` ticks, each as one
/// `data: {json}\n\n` SSE frame, plus a leading keep-alive comment.
fn sse_route(n: u64) -> Router {
    Router::new().route(
        "/events",
        get(move || async move {
            let frames = stream::iter(
                std::iter::once(Ok::<_, Infallible>(": keep-alive\n\n".to_string())).chain(
                    (0..n).map(|seq| {
                        let doc = serde_json::to_string(&Tick { seq }).expect("encode");
                        Ok::<_, Infallible>(format!("event: tick\ndata: {doc}\n\n"))
                    }),
                ),
            );
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, SSE_CONTENT_TYPE)
                .body(Body::from_stream(frames))
                .expect("response")
        }),
    )
}

#[tokio::test]
async fn flux_streams_sse_data_frames() {
    let base = spawn_server(sse_route(4)).await;
    let client = WebClientBuilder::new(&base).build();

    let ticks = client
        .get()
        .uri("/events")
        .retrieve()
        .body_to_flux::<Tick>()
        .collect_list()
        .block()
        .await
        .expect("flux ok")
        .expect("list");
    assert_eq!(
        ticks,
        (0..4).map(|seq| Tick { seq }).collect::<Vec<_>>(),
        "the keep-alive comment block is skipped; each data frame decodes"
    );
}

/// A handler that delays each NDJSON line slightly, exercising the
/// chunk-by-chunk lazy decode path (the producer's chunks arrive over
/// time, and the `Flux` advances as they do). Total delay stays under the
/// 200 ms budget.
fn slow_ndjson_route(n: u64) -> Router {
    Router::new().route(
        "/slow",
        get(move || async move {
            let lines = stream::unfold(0u64, move |i| async move {
                if i >= n {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                let doc = serde_json::to_string(&Tick { seq: i }).expect("encode");
                Some((Ok::<_, Infallible>(format!("{doc}\n")), i + 1))
            });
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, NDJSON_CONTENT_TYPE)
                .body(Body::from_stream(lines))
                .expect("response")
        }),
    )
}

#[tokio::test]
async fn flux_decodes_lazily_chunk_by_chunk() {
    let base = spawn_server(slow_ndjson_route(6)).await;
    let client = WebClientBuilder::new(&base).build();

    // Take only the first three even though the server would emit six —
    // the `Flux` is lazy, so we never need to wait for the whole stream.
    let first_three = client
        .get()
        .uri("/slow")
        .retrieve()
        .body_to_flux::<Tick>()
        .take(3)
        .collect_list()
        .block()
        .await
        .expect("flux ok")
        .expect("list");
    assert_eq!(
        first_three,
        (0..3).map(|seq| Tick { seq }).collect::<Vec<_>>()
    );
}

// --- problem decode -------------------------------------------------------

#[tokio::test]
async fn mono_problem_decode() {
    let app = Router::new().route(
        "/x",
        get(|| async {
            let pd = ProblemDetail::not_found("missing");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, PROBLEM_CONTENT_TYPE)],
                serde_json::to_string(&pd).expect("encode"),
            )
        }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    let err = client
        .get()
        .uri("/x")
        .retrieve()
        .body_to_mono::<User>()
        .block()
        .await
        .expect_err("non-2xx becomes terminal error");
    assert_eq!(err.status, 404);
    assert_eq!(err.code, TYPE_NOT_FOUND);
    assert_eq!(err.title, "Not Found");
    assert_eq!(err.detail, "missing");
}

#[tokio::test]
async fn flux_problem_decode_is_terminal_error() {
    let app = Router::new().route(
        "/ticks",
        get(|| async {
            let pd = ProblemDetail::bad_request("bad stream");
            (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, PROBLEM_CONTENT_TYPE)],
                serde_json::to_string(&pd).expect("encode"),
            )
        }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    let err = client
        .get()
        .uri("/ticks")
        .retrieve()
        .body_to_flux::<Tick>()
        .collect_list()
        .block()
        .await
        .expect_err("non-2xx becomes terminal error on the Flux");
    assert_eq!(err.status, 400);
    assert_eq!(err.detail, "bad stream");
}

#[tokio::test]
async fn non_problem_error_body_wraps_raw_detail() {
    let app = Router::new().route("/x", get(|| async { (StatusCode::BAD_REQUEST, "boom") }));
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    let err = client
        .get()
        .uri("/x")
        .retrieve()
        .body_to_mono::<User>()
        .block()
        .await
        .expect_err("error");
    assert_eq!(err.status, 400);
    assert_eq!(err.title, "Bad Request");
    assert_eq!(err.code, "");
    assert_eq!(err.detail, "boom");
}

// --- exchange: raw reactive response --------------------------------------

#[tokio::test]
async fn exchange_exposes_status_and_headers_without_raising() {
    let app = Router::new().route(
        "/health",
        get(|| async { (StatusCode::OK, [("x-custom", "yes")], r#"{"status":"UP"}"#) }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    let resp = client
        .get()
        .uri("/health")
        .retrieve()
        .exchange()
        .block()
        .await
        .expect("mono ok")
        .expect("present");
    assert_eq!(resp.status(), 200);
    assert!(resp.is_success());
    assert_eq!(
        resp.headers().get("x-custom").and_then(|v| v.to_str().ok()),
        Some("yes")
    );
    assert!(resp.problem().is_none());

    #[derive(Deserialize)]
    struct Health {
        status: String,
    }
    let health: Health = resp.body_json().expect("decode body");
    assert_eq!(health.status, "UP");
}

#[tokio::test]
async fn exchange_does_not_raise_on_non_2xx() {
    let app = Router::new().route(
        "/x",
        get(|| async {
            let pd = ProblemDetail::not_found("gone");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, PROBLEM_CONTENT_TYPE)],
                serde_json::to_string(&pd).expect("encode"),
            )
        }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    // exchange() returns the response even for 404 — the caller decides.
    let resp = client
        .get()
        .uri("/x")
        .retrieve()
        .exchange()
        .block()
        .await
        .expect("mono ok")
        .expect("present");
    assert_eq!(resp.status(), 404);
    assert!(!resp.is_success());
    let problem = resp.problem().expect("decoded problem");
    assert_eq!(problem.status, 404);
    assert_eq!(problem.detail, "gone");
}

// --- correlation propagation ----------------------------------------------

#[tokio::test]
async fn correlation_id_propagates_from_task_local() {
    let seen: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));
    let captor = seen.clone();
    let app = Router::new().route(
        "/x",
        get(move |headers: HeaderMap| {
            let captor = captor.clone();
            async move {
                *captor.lock().expect("lock") = Some(headers);
                Json(Tick { seq: 1 })
            }
        }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base).build();

    with_correlation_id("web-abc", async {
        client
            .get()
            .uri("/x")
            .retrieve()
            .body_to_mono::<Tick>()
            .block()
            .await
            .expect("mono ok");
    })
    .await;

    let headers = seen.lock().expect("lock").take().expect("headers");
    assert_eq!(
        headers
            .get("x-correlation-id")
            .and_then(|v| v.to_str().ok()),
        Some("web-abc")
    );
    // Accept defaults to application/json, as with the eager client.
    assert_eq!(
        headers.get("accept").and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
}

// --- builder behaviour ----------------------------------------------------

#[tokio::test]
async fn default_and_per_request_headers_both_sent() {
    let seen: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));
    let captor = seen.clone();
    let app = Router::new().route(
        "/x",
        get(move |headers: HeaderMap| {
            let captor = captor.clone();
            async move {
                *captor.lock().expect("lock") = Some(headers);
                Json(Tick { seq: 1 })
            }
        }),
    );
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new(&base)
        .with_header("X-Tenant", "acme")
        .build();

    client
        .get()
        .uri("/x")
        .header("X-Request", "r1")
        .retrieve()
        .body_to_mono::<Tick>()
        .block()
        .await
        .expect("mono ok");

    let headers = seen.lock().expect("lock").take().expect("headers");
    assert_eq!(
        headers.get("x-tenant").and_then(|v| v.to_str().ok()),
        Some("acme")
    );
    assert_eq!(
        headers.get("x-request").and_then(|v| v.to_str().ok()),
        Some("r1")
    );
}

#[tokio::test]
async fn absolute_uri_overrides_base() {
    // A server reachable only at its absolute URL; the client's base is
    // a deliberately-wrong host that an absolute `uri` must bypass.
    let app = Router::new().route("/abs", get(|| async { Json(Tick { seq: 7 }) }));
    let base = spawn_server(app).await;
    let client = WebClientBuilder::new("http://127.0.0.1:1").build();

    let out = client
        .get()
        .uri(format!("{base}/abs"))
        .retrieve()
        .body_to_mono::<Tick>()
        .block()
        .await
        .expect("mono ok")
        .expect("present");
    assert_eq!(out.seq, 7);
}

#[tokio::test]
async fn invalid_url_surfaces_as_terminal_error() {
    let client = WebClientBuilder::new("not a url").build();
    let err = client
        .get()
        .uri("/x")
        .retrieve()
        .body_to_mono::<Tick>()
        .block()
        .await
        .expect_err("invalid url is a terminal error");
    assert_eq!(err.status, 400);
}

#[tokio::test]
async fn timeout_yields_terminal_error() {
    // Bind then drop a listener so the port is (almost certainly) dead.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    let client = WebClientBuilder::new(format!("http://{addr}"))
        .with_timeout(Duration::from_millis(50))
        .build();
    let err = client
        .get()
        .uri("/x")
        .retrieve()
        .body_to_mono::<Tick>()
        .block()
        .await
        .expect_err("transport failure is a terminal error");
    // A connection failure maps to the 502 gateway error.
    assert_eq!(err.status, 502);
}
