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

//! Problem-rendering [`Path<T>`] and [`Query<T>`] extractors — drop-in
//! replacements for `axum::extract::{Path, Query}` that turn an extraction
//! failure into an RFC 9457 `application/problem+json` response instead of
//! axum's plain-text rejection.
//!
//! A malformed path segment (e.g. a non-UUID where a `Uuid` is expected) or a
//! missing/un-parseable query parameter would otherwise escape the framework's
//! problem surface as a bare `400`/`text/plain` body. These extractors keep the
//! whole error surface consistent — the Rust analog of Spring's
//! `MethodArgumentTypeMismatchException` / `MissingServletRequestParameterException`
//! being rendered by the same `@ControllerAdvice` as every other error.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{FromRequestParts, Path as AxumPath, Query as AxumQuery};
use axum::response::{IntoResponse, Response};
use firefly_kernel::FireflyError;
use http::request::Parts;
use serde::de::DeserializeOwned;

use crate::problem::WebError;

/// An axum path extractor that renders a rejection as an RFC 9457 problem.
///
/// Use it exactly like [`axum::extract::Path`], swapping the import; the inner
/// `T` is the deserialized path parameter(s). A path segment that fails to
/// deserialize into `T` rejects with a **400 Bad Request**
/// `application/problem+json` instead of axum's plain-text body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Path<T>(pub T);

impl<T> Path<T> {
    /// Consumes the wrapper, returning the extracted value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[axum::async_trait]
impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match AxumPath::<T>::from_request_parts(parts, state).await {
            Ok(AxumPath(value)) => Ok(Path(value)),
            Err(rejection) => Err(path_problem(rejection)),
        }
    }
}

/// An axum query extractor that renders a rejection as an RFC 9457 problem.
///
/// Use it exactly like [`axum::extract::Query`]. A missing required parameter or
/// a value that fails to deserialize into `T` rejects with a **400 Bad Request**
/// `application/problem+json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Query<T>(pub T);

impl<T> Query<T> {
    /// Consumes the wrapper, returning the extracted value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[axum::async_trait]
impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match AxumQuery::<T>::from_request_parts(parts, state).await {
            Ok(AxumQuery(value)) => Ok(Query(value)),
            Err(rejection) => Err(query_problem(rejection)),
        }
    }
}

fn path_problem(rejection: PathRejection) -> Response {
    WebError::from(FireflyError::bad_request(rejection.body_text())).into_response()
}

fn query_problem(rejection: QueryRejection) -> Response {
    WebError::from(FireflyError::bad_request(rejection.body_text())).into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use firefly_kernel::PROBLEM_CONTENT_TYPE;
    use http::{Request, StatusCode};
    use serde::Deserialize;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::{Path, Query};

    #[derive(Deserialize)]
    struct Filter {
        owner: String,
    }

    async fn by_id(Path(id): Path<Uuid>) -> String {
        id.to_string()
    }

    async fn list(Query(filter): Query<Filter>) -> String {
        filter.owner
    }

    fn app() -> Router {
        Router::new()
            .route("/items/:id", get(by_id))
            .route("/items", get(list))
    }

    async fn call(uri: &str) -> (StatusCode, Option<String>) {
        let res = app()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let ct = res
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        (status, ct)
    }

    #[tokio::test]
    async fn valid_path_and_query_reach_the_handler() {
        let (status, _) = call(&format!("/items/{}", Uuid::new_v4())).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call("/items?owner=ada").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_path_rejects_with_400_problem() {
        let (status, ct) = call("/items/not-a-uuid").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(ct.as_deref(), Some(PROBLEM_CONTENT_TYPE));
    }

    #[tokio::test]
    async fn missing_required_query_rejects_with_400_problem() {
        let (status, ct) = call("/items").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(ct.as_deref(), Some(PROBLEM_CONTENT_TYPE));
    }
}
