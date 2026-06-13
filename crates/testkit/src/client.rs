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

//! In-process HTTP test client over an axum [`Router`].
//!
//! [`TestClient`] is the Rust analog of pyfly's `PyFlyTestClient`: it drives a
//! [`Router`] *in process* via [`tower::ServiceExt::oneshot`] — no socket is
//! bound and no server task is spawned — and hands back a [`TestResponse`] with
//! fluent, chainable assertions ([`TestResponse::assert_status`],
//! [`TestResponse::json`], [`TestResponse::assert_header`],
//! [`TestResponse::assert_body_contains`]).
//!
//! Every method comes in two flavours:
//!
//! - **async** ([`TestClient::get`], [`TestClient::post`], …) — call them from
//!   inside an existing `#[tokio::test]`.
//! - **blocking** ([`TestClient::get_blocking`], …) — drive a request on an
//!   internal current-thread runtime, so a plain `#[test]` reads exactly like
//!   pyfly's synchronous `client.get(...)`.
//!
//! Available only with the `web` feature.
//!
//! ```
//! # #[cfg(feature = "web")] {
//! use axum::{routing::get, Json, Router};
//! use firefly_testkit::TestClient;
//!
//! let app = Router::new().route("/ping", get(|| async { Json(serde_json::json!({ "ok": true })) }));
//! let client = TestClient::new(app);
//!
//! client
//!     .get_blocking("/ping")
//!     .assert_status(200)
//!     .assert_header("content-type", "application/json")
//!     .assert_json_eq(&serde_json::json!({ "ok": true }));
//! # }
//! ```

use axum::body::Body;
use axum::Router;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::OnceLock;
use tower::ServiceExt;

/// An in-process HTTP client that drives an axum [`Router`].
///
/// Clone-cheap: the wrapped [`Router`] is cloned per request (axum routers are
/// `Clone`), so a single `TestClient` can serve many requests. The Rust analog
/// of pyfly's `PyFlyTestClient`.
#[derive(Clone)]
pub struct TestClient {
    router: Router,
}

impl TestClient {
    /// Wrap a built axum [`Router`].
    ///
    /// The router must already have its application state applied (i.e. be a
    /// `Router<()>`), the same shape `axum::serve` expects.
    #[must_use]
    pub fn new(router: Router) -> Self {
        Self { router }
    }

    /// Send a request with an arbitrary [`Method`], URI, optional `content-type`,
    /// and raw body, awaiting the response.
    ///
    /// This is the low-level primitive the typed `get`/`post`/… helpers build on.
    ///
    /// # Panics
    /// Panics (failing the test) if the request cannot be built or the router's
    /// infallible service returns an error.
    pub async fn request(
        &self,
        method: Method,
        uri: &str,
        content_type: Option<&str>,
        body: Body,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(ct) = content_type {
            builder = builder.header(http::header::CONTENT_TYPE, ct);
        }
        let req = builder
            .body(body)
            .expect("TestClient: failed to build request");
        let res = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("TestClient: router service returned an error");
        TestResponse::from_response(res).await
    }

    /// Send a `GET` request and await the response.
    pub async fn get(&self, uri: &str) -> TestResponse {
        self.request(Method::GET, uri, None, Body::empty()).await
    }

    /// Send a `DELETE` request and await the response.
    pub async fn delete(&self, uri: &str) -> TestResponse {
        self.request(Method::DELETE, uri, None, Body::empty()).await
    }

    /// Send a `POST` request with a JSON body and await the response.
    ///
    /// The `body` is serialized with `serde_json` and the `content-type` header
    /// is set to `application/json`.
    ///
    /// # Panics
    /// Panics if `body` cannot be serialized.
    pub async fn post<T: serde::Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestResponse {
        self.request(Method::POST, uri, Some(JSON), json_body(body))
            .await
    }

    /// Send a `PUT` request with a JSON body and await the response.
    ///
    /// # Panics
    /// Panics if `body` cannot be serialized.
    pub async fn put<T: serde::Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestResponse {
        self.request(Method::PUT, uri, Some(JSON), json_body(body))
            .await
    }

    /// Send a `PATCH` request with a JSON body and await the response.
    ///
    /// # Panics
    /// Panics if `body` cannot be serialized.
    pub async fn patch<T: serde::Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestResponse {
        self.request(Method::PATCH, uri, Some(JSON), json_body(body))
            .await
    }

    /// Send a `POST` request with no body and await the response.
    pub async fn post_empty(&self, uri: &str) -> TestResponse {
        self.request(Method::POST, uri, None, Body::empty()).await
    }

    // ------------------------------------------------------------------
    // Blocking convenience wrappers — read like pyfly's sync client.
    // ------------------------------------------------------------------

    /// Blocking [`get`](TestClient::get): drive the request on an internal
    /// current-thread runtime.
    ///
    /// # Panics
    /// Panics if called from inside a Tokio runtime (use the async `get`
    /// instead) — the same constraint as any blocking-on-async bridge.
    #[must_use]
    pub fn get_blocking(&self, uri: &str) -> TestResponse {
        block_on(self.get(uri))
    }

    /// Blocking [`delete`](TestClient::delete).
    ///
    /// # Panics
    /// Panics if called from inside a Tokio runtime.
    #[must_use]
    pub fn delete_blocking(&self, uri: &str) -> TestResponse {
        block_on(self.delete(uri))
    }

    /// Blocking [`post`](TestClient::post).
    ///
    /// # Panics
    /// Panics if `body` cannot be serialized, or if called from inside a Tokio
    /// runtime.
    #[must_use]
    pub fn post_blocking<T: serde::Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestResponse {
        block_on(self.post(uri, body))
    }

    /// Blocking [`put`](TestClient::put).
    ///
    /// # Panics
    /// Panics if `body` cannot be serialized, or if called from inside a Tokio
    /// runtime.
    #[must_use]
    pub fn put_blocking<T: serde::Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestResponse {
        block_on(self.put(uri, body))
    }

    /// Blocking [`patch`](TestClient::patch).
    ///
    /// # Panics
    /// Panics if `body` cannot be serialized, or if called from inside a Tokio
    /// runtime.
    #[must_use]
    pub fn patch_blocking<T: serde::Serialize + ?Sized>(
        &self,
        uri: &str,
        body: &T,
    ) -> TestResponse {
        block_on(self.patch(uri, body))
    }

    /// Blocking [`post_empty`](TestClient::post_empty).
    ///
    /// # Panics
    /// Panics if called from inside a Tokio runtime.
    #[must_use]
    pub fn post_empty_blocking(&self, uri: &str) -> TestResponse {
        block_on(self.post_empty(uri))
    }
}

const JSON: &str = "application/json";

/// Serialize `body` into a JSON request [`Body`], panicking on failure.
fn json_body<T: serde::Serialize + ?Sized>(body: &T) -> Body {
    let bytes = serde_json::to_vec(body).expect("TestClient: failed to serialize JSON body");
    Body::from(bytes)
}

/// Run `fut` to completion on a shared internal current-thread runtime.
///
/// A single runtime is reused across blocking calls so repeated `*_blocking`
/// requests don't each pay runtime-startup cost.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("TestClient: failed to build internal Tokio runtime")
    });
    rt.block_on(fut)
}

/// A captured HTTP response with fluent, chainable assertions.
///
/// Status, headers, and the fully-buffered body are read eagerly so assertions
/// are synchronous and re-runnable. The Rust analog of pyfly's `TestResponse`.
#[derive(Debug, Clone)]
pub struct TestResponse {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl TestResponse {
    /// Drain an axum [`Response`](axum::response::Response) into a buffered
    /// `TestResponse` (status + lower-cased headers + full body bytes).
    async fn from_response(res: axum::response::Response) -> Self {
        let status = res.status();
        let headers = res
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = res
            .into_body()
            .collect()
            .await
            .expect("TestClient: failed to read response body")
            .to_bytes()
            .to_vec();
        Self {
            status,
            headers,
            body,
        }
    }

    /// The numeric HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status.as_u16()
    }

    /// The raw response body bytes.
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        &self.body
    }

    /// The response body as a UTF-8 string (lossily decoded).
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The first value of the header `name` (case-insensitive), if present.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Parse the body as JSON into a `T`.
    ///
    /// # Panics
    /// Panics (failing the test) if the body is not valid JSON for `T`. To get
    /// the loosely-typed tree instead, parse into [`serde_json::Value`].
    #[must_use]
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!(
                "TestResponse::json: body is not valid JSON ({err}); body: {}",
                self.text()
            )
        })
    }

    /// Assert the status code equals `expected`. Returns `self` for chaining.
    ///
    /// `expected` is a `u16` so call sites read `assert_status(200)`.
    ///
    /// # Panics
    /// Panics if the status differs, including the body to aid debugging.
    pub fn assert_status(&self, expected: u16) -> &Self {
        assert_eq!(
            self.status(),
            expected,
            "expected status {expected}, got {} (body: {})",
            self.status(),
            self.text()
        );
        self
    }

    /// Assert the response was a `2xx` success. Returns `self` for chaining.
    ///
    /// # Panics
    /// Panics if the status is not in the `200..=299` range.
    pub fn assert_success(&self) -> &Self {
        assert!(
            self.status.is_success(),
            "expected a 2xx status, got {} (body: {})",
            self.status(),
            self.text()
        );
        self
    }

    /// Assert header `name` (case-insensitive) is present and equals `value`.
    /// Returns `self` for chaining.
    ///
    /// # Panics
    /// Panics if the header is absent or its value differs.
    pub fn assert_header(&self, name: &str, value: &str) -> &Self {
        match self.header(name) {
            None => panic!("header {name:?} not found; headers: {:?}", self.headers),
            Some(got) => assert_eq!(
                got, value,
                "expected header {name:?} = {value:?}, got {got:?}"
            ),
        }
        self
    }

    /// Assert header `name` (case-insensitive) is present (any value). Returns
    /// `self` for chaining.
    ///
    /// # Panics
    /// Panics if the header is absent.
    pub fn assert_header_present(&self, name: &str) -> &Self {
        assert!(
            self.header(name).is_some(),
            "header {name:?} not found; headers: {:?}",
            self.headers
        );
        self
    }

    /// Assert the (UTF-8) body contains `needle`. Returns `self` for chaining.
    ///
    /// # Panics
    /// Panics if the body does not contain `needle`.
    pub fn assert_body_contains(&self, needle: &str) -> &Self {
        let text = self.text();
        assert!(
            text.contains(needle),
            "body does not contain {needle:?}; body: {text}"
        );
        self
    }

    /// Assert the JSON body equals `expected` exactly. Returns `self` for
    /// chaining.
    ///
    /// # Panics
    /// Panics if the body is not valid JSON or does not equal `expected`.
    pub fn assert_json_eq(&self, expected: &serde_json::Value) -> &Self {
        let actual: serde_json::Value = self.json();
        assert_eq!(&actual, expected, "JSON body mismatch");
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
    struct Echo {
        message: String,
    }

    fn app() -> Router {
        Router::new()
            .route(
                "/ping",
                get(|| async {
                    (
                        [("x-custom", "yes")],
                        Json(serde_json::json!({ "ok": true })),
                    )
                }),
            )
            .route(
                "/echo",
                post(|Json(body): Json<Echo>| async move { Json(body) }),
            )
            .route(
                "/items/:id",
                get(
                    |axum::extract::Path(id): axum::extract::Path<String>| async move {
                        Json(serde_json::json!({ "id": id }))
                    },
                ),
            )
            .route(
                "/created",
                post(|| async { (StatusCode::CREATED, "made it") }),
            )
            .route(
                "/boom",
                get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "nope") }),
            )
            .route(
                "/replace",
                axum::routing::put(|Json(body): Json<Echo>| async move { Json(body) }),
            )
            .route(
                "/patch",
                axum::routing::patch(|Json(body): Json<Echo>| async move { Json(body) }),
            )
            .route(
                "/remove",
                axum::routing::delete(|| async { StatusCode::NO_CONTENT }),
            )
    }

    #[tokio::test]
    async fn get_returns_json_and_headers() {
        let client = TestClient::new(app());
        let res = client.get("/ping").await;
        res.assert_status(200)
            .assert_success()
            .assert_header("content-type", "application/json")
            .assert_header_present("x-custom")
            .assert_header("x-custom", "yes")
            .assert_json_eq(&serde_json::json!({ "ok": true }));
        assert_eq!(res.header("X-CUSTOM"), Some("yes")); // case-insensitive lookup
    }

    #[tokio::test]
    async fn post_round_trips_json_body() {
        let client = TestClient::new(app());
        let res = client
            .post(
                "/echo",
                &Echo {
                    message: "hi".into(),
                },
            )
            .await;
        res.assert_status(200);
        let echoed: Echo = res.json();
        assert_eq!(
            echoed,
            Echo {
                message: "hi".into()
            }
        );
    }

    #[tokio::test]
    async fn put_and_patch_round_trip() {
        let client = TestClient::new(app());
        let put = client
            .put(
                "/replace",
                &Echo {
                    message: "p".into(),
                },
            )
            .await;
        assert_eq!(
            put.json::<Echo>(),
            Echo {
                message: "p".into()
            }
        );
        let patch = client
            .patch(
                "/patch",
                &Echo {
                    message: "q".into(),
                },
            )
            .await;
        assert_eq!(
            patch.json::<Echo>(),
            Echo {
                message: "q".into()
            }
        );
    }

    #[tokio::test]
    async fn delete_and_post_empty() {
        let client = TestClient::new(app());
        client.delete("/remove").await.assert_status(204);
        client.post_empty("/created").await.assert_status(201);
    }

    #[tokio::test]
    async fn path_params_resolve() {
        let client = TestClient::new(app());
        client
            .get("/items/abc")
            .await
            .assert_status(200)
            .assert_json_eq(&serde_json::json!({ "id": "abc" }));
    }

    #[tokio::test]
    async fn client_is_reusable_across_requests() {
        let client = TestClient::new(app());
        for _ in 0..3 {
            client.get("/ping").await.assert_status(200);
        }
    }

    #[tokio::test]
    async fn error_status_and_body_text() {
        let client = TestClient::new(app());
        let res = client.get("/boom").await;
        res.assert_status(500).assert_body_contains("nope");
        assert_eq!(res.text(), "nope");
        assert_eq!(res.body_bytes(), b"nope");
    }

    #[tokio::test]
    #[should_panic(expected = "expected status 200, got 500")]
    async fn assert_status_mismatch_includes_body() {
        let client = TestClient::new(app());
        let _ = client.get("/boom").await.assert_status(200);
    }

    #[tokio::test]
    #[should_panic(expected = "header \"missing\" not found")]
    async fn assert_header_missing_panics() {
        let client = TestClient::new(app());
        let _ = client.get("/ping").await.assert_header("missing", "x");
    }

    #[tokio::test]
    #[should_panic(expected = "body does not contain")]
    async fn assert_body_contains_failure() {
        let client = TestClient::new(app());
        let _ = client.get("/boom").await.assert_body_contains("absent");
    }

    // The blocking wrappers run outside any ambient runtime.
    #[test]
    fn blocking_wrappers_work() {
        let client = TestClient::new(app());
        client
            .get_blocking("/ping")
            .assert_status(200)
            .assert_json_eq(&serde_json::json!({ "ok": true }));
        client
            .post_blocking(
                "/echo",
                &Echo {
                    message: "b".into(),
                },
            )
            .assert_status(200)
            .assert_json_eq(&serde_json::json!({ "message": "b" }));
        client
            .put_blocking(
                "/replace",
                &Echo {
                    message: "c".into(),
                },
            )
            .assert_status(200);
        client
            .patch_blocking(
                "/patch",
                &Echo {
                    message: "d".into(),
                },
            )
            .assert_status(200);
        client.delete_blocking("/remove").assert_status(204);
        client.post_empty_blocking("/created").assert_status(201);
    }

    #[test]
    fn client_is_clone() {
        let client = TestClient::new(app());
        let cloned = client.clone();
        cloned.get_blocking("/ping").assert_status(200);
        client.get_blocking("/ping").assert_status(200);
    }
}
