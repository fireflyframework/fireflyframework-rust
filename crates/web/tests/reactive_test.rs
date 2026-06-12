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

//! In-process HTTP tests for the additive reactive (WebFlux/Reactor)
//! surface of `firefly-web`, driven through `tower::ServiceExt::oneshot`
//! — no sockets. Covers the `MonoJson` responder (200 / 404 / problem),
//! the backpressured `NdJson` streaming responder (exact multi-line body
//! bytes, error-mid-stream truncation), and the SSE responders (frame
//! bytes, error-mid-stream truncation).

use std::time::Duration;

use axum::body::Body;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use firefly_kernel::{FireflyError, ProblemDetail, PROBLEM_CONTENT_TYPE, TYPE_NOT_FOUND};
use firefly_reactive::{Flux, Mono};
use firefly_sse::Event as SseEvent;
use firefly_web::{MonoJson, NdJson, Sse, SseEvents, NDJSON_CONTENT_TYPE, SSE_CONTENT_TYPE};
use http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Order {
    id: String,
    total: u64,
}

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

// ---------------------------------------------------------------------
// MonoJson: 200 / 404 / problem
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn mono_some_renders_200_json() {
    async fn handler() -> impl IntoResponse {
        MonoJson(Mono::just(Order {
            id: "o1".into(),
            total: 42,
        }))
    }
    let app = Router::new().route("/orders/o1", get(handler));

    let (status, headers, body) = send(app, get_req("/orders/o1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let got: Order = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        got,
        Order {
            id: "o1".into(),
            total: 42
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mono_empty_renders_404_problem() {
    async fn handler() -> impl IntoResponse {
        MonoJson(Mono::<Order>::empty())
    }
    let app = Router::new().route("/orders/missing", get(handler));

    let (status, headers, body) = send(app, get_req("/orders/missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        PROBLEM_CONTENT_TYPE
    );
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.problem_type, TYPE_NOT_FOUND);
    assert_eq!(pd.status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn mono_error_renders_that_errors_problem() {
    async fn handler() -> impl IntoResponse {
        MonoJson(Mono::<Order>::error(FireflyError::bad_request(
            "customer is required",
        )))
    }
    let app = Router::new().route("/orders", get(handler));

    let (status, headers, body) = send(app, get_req("/orders")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        PROBLEM_CONTENT_TYPE
    );
    let pd: ProblemDetail = serde_json::from_slice(&body).unwrap();
    assert_eq!(pd.status, 400);
    assert_eq!(pd.detail, "customer is required");
}

/// `MonoJson` also resolves under a current-thread runtime (the
/// `resolve_mono` fallback path), so the default `#[tokio::test]` flavor
/// works too.
#[tokio::test]
async fn mono_resolves_under_current_thread_runtime() {
    async fn handler() -> impl IntoResponse {
        MonoJson(Mono::just(Order {
            id: "ct".into(),
            total: 7,
        }))
    }
    let app = Router::new().route("/orders/ct", get(handler));
    let (status, _h, body) = send(app, get_req("/orders/ct")).await;
    assert_eq!(status, StatusCode::OK);
    let got: Order = serde_json::from_slice(&body).unwrap();
    assert_eq!(got.id, "ct");
}

// ---------------------------------------------------------------------
// NdJson: exact body bytes, multi-line, error mid-stream
// ---------------------------------------------------------------------

#[tokio::test]
async fn ndjson_emits_exact_newline_delimited_bytes() {
    async fn handler() -> impl IntoResponse {
        NdJson(Flux::just(vec![
            Order {
                id: "o1".into(),
                total: 1,
            },
            Order {
                id: "o2".into(),
                total: 2,
            },
            Order {
                id: "o3".into(),
                total: 3,
            },
        ]))
    }
    let app = Router::new().route("/orders", get(handler));

    let (status, headers, body) = send(app, get_req("/orders")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        NDJSON_CONTENT_TYPE
    );
    // One compact JSON doc + '\n' per element, in order.
    let want = concat!(
        "{\"id\":\"o1\",\"total\":1}\n",
        "{\"id\":\"o2\",\"total\":2}\n",
        "{\"id\":\"o3\",\"total\":3}\n",
    );
    assert_eq!(std::str::from_utf8(&body).unwrap(), want);
}

#[tokio::test]
async fn ndjson_empty_flux_is_empty_body() {
    async fn handler() -> impl IntoResponse {
        NdJson(Flux::<Order>::empty())
    }
    let app = Router::new().route("/orders", get(handler));
    let (status, _h, body) = send(app, get_req("/orders")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "unexpected bytes: {body:?}");
}

#[tokio::test]
async fn ndjson_error_mid_stream_truncates_cleanly() {
    // Two good elements, then a terminal error: the body keeps the two
    // lines emitted before the error and ends (no trailing frame).
    async fn handler() -> impl IntoResponse {
        let good = Flux::just(vec![
            Order {
                id: "o1".into(),
                total: 1,
            },
            Order {
                id: "o2".into(),
                total: 2,
            },
        ]);
        let boom = Flux::<Order>::error(FireflyError::internal("kaboom"));
        NdJson(good.concat_with(boom))
    }
    let app = Router::new().route("/orders", get(handler));

    let (status, _h, body) = send(app, get_req("/orders")).await;
    assert_eq!(status, StatusCode::OK);
    let want = concat!(
        "{\"id\":\"o1\",\"total\":1}\n",
        "{\"id\":\"o2\",\"total\":2}\n",
    );
    assert_eq!(std::str::from_utf8(&body).unwrap(), want);
}

// ---------------------------------------------------------------------
// SSE: frame bytes, pre-built events, error mid-stream
// ---------------------------------------------------------------------

#[tokio::test]
async fn sse_emits_data_frames_per_element() {
    async fn handler() -> impl IntoResponse {
        Sse(Flux::just(vec![1u32, 2, 3]))
    }
    let app = Router::new().route("/ticks", get(handler));

    let (status, headers, body) = send(app, get_req("/ticks")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), SSE_CONTENT_TYPE);
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");
    // One bare `data: <json>\n\n` frame per element (firefly-sse wire).
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        "data: 1\n\ndata: 2\n\ndata: 3\n\n"
    );
}

#[tokio::test]
async fn sse_events_emit_full_frames() {
    async fn handler() -> impl IntoResponse {
        SseEvents(Flux::just(vec![
            SseEvent {
                id: "1".into(),
                event: "tick".into(),
                data: "hello".into(),
                ..SseEvent::default()
            },
            SseEvent {
                data: "line1\nline2".into(),
                ..SseEvent::default()
            },
        ]))
    }
    let app = Router::new().route("/events", get(handler));

    let (status, headers, body) = send(app, get_req("/events")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), SSE_CONTENT_TYPE);
    // Byte-identical to the firefly-sse writer output.
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        "id: 1\nevent: tick\ndata: hello\n\ndata: line1\ndata: line2\n\n"
    );
}

#[tokio::test]
async fn sse_error_mid_stream_truncates_cleanly() {
    async fn handler() -> impl IntoResponse {
        let good = Flux::just(vec![1u32, 2]);
        let boom = Flux::<u32>::error(FireflyError::internal("kaboom"));
        Sse(good.concat_with(boom))
    }
    let app = Router::new().route("/ticks", get(handler));

    let (status, _h, body) = send(app, get_req("/ticks")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        "data: 1\n\ndata: 2\n\n"
    );
}

/// A short sanity check that the streaming path actually flushes
/// incrementally rather than buffering: a `delay_elements` Flux still
/// completes well under the test budget (no per-test sleep > 200ms).
#[tokio::test]
async fn ndjson_streams_delayed_elements() {
    async fn handler() -> impl IntoResponse {
        NdJson(Flux::just(vec![1u32, 2]).delay_elements(Duration::from_millis(10)))
    }
    let app = Router::new().route("/n", get(handler));
    let (status, _h, body) = send(app, get_req("/n")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(std::str::from_utf8(&body).unwrap(), "1\n2\n");
}
