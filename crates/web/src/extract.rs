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
use firefly_validators::bean::Validate;
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

/// A path extractor that runs the bound type's declarative [`Validate`]
/// constraints after deserialization — `@Valid @PathVariable` for a structured
/// path binding.
///
/// Extracts exactly like [`Path<T>`] (a malformed path → **400** problem), then
/// runs `T::validate()`; a constraint failure rejects with a **422**
/// `application/problem+json` carrying the structured violations. Use it for a
/// multi-segment path object whose parts have constraints:
///
/// ```ignore
/// #[derive(Deserialize, Validate)]
/// struct Coord { #[validate(range(min = -90, max = 90))] lat: f64, lng: f64 }
///
/// async fn cell(ValidPath(c): ValidPath<Coord>) -> WebResult<String> { /* … */ }
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidPath<T>(pub T);

impl<T> ValidPath<T> {
    /// Consumes the wrapper, returning the validated value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[axum::async_trait]
impl<T, S> FromRequestParts<S> for ValidPath<T>
where
    T: DeserializeOwned + Validate + Send,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path(value) = Path::<T>::from_request_parts(parts, state).await?;
        match value.validate() {
            Ok(()) => Ok(ValidPath(value)),
            Err(errors) => Err(WebError::from(FireflyError::from(errors)).into_response()),
        }
    }
}

/// A query extractor that runs the bound type's declarative [`Validate`]
/// constraints after deserialization — `@Valid` on a query/filter object, the
/// common Spring `@ModelAttribute` validation case.
///
/// Extracts exactly like [`Query<T>`] (a missing/ill-typed parameter → **400**
/// problem), then runs `T::validate()`; a constraint failure rejects with a
/// **422** `application/problem+json` carrying the structured violations:
///
/// ```ignore
/// #[derive(Deserialize, Validate)]
/// struct Search { #[validate(not_empty)] q: String, #[validate(range(min = 1, max = 100))] size: u32 }
///
/// async fn search(ValidQuery(s): ValidQuery<Search>) -> WebResult<String> { /* … */ }
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidQuery<T>(pub T);

impl<T> ValidQuery<T> {
    /// Consumes the wrapper, returning the validated value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[axum::async_trait]
impl<T, S> FromRequestParts<S> for ValidQuery<T>
where
    T: DeserializeOwned + Validate + Send,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state).await?;
        match value.validate() {
            Ok(()) => Ok(ValidQuery(value)),
            Err(errors) => Err(WebError::from(FireflyError::from(errors)).into_response()),
        }
    }
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

    use firefly_validators::bean::{Validate, ValidationError, ValidationErrors};

    use super::{Path, Query, ValidQuery};

    #[derive(Deserialize)]
    struct Filter {
        owner: String,
    }

    /// A query/filter object with a declarative constraint, validated hand-rolled
    /// here so the test does not depend on `firefly-macros`.
    #[derive(Deserialize)]
    struct Search {
        q: String,
    }

    impl Validate for Search {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            if self.q.trim().is_empty() {
                errors.push(ValidationError::new("q", "not_empty", "must not be empty"));
            }
            errors.into_result()
        }
    }

    async fn by_id(Path(id): Path<Uuid>) -> String {
        id.to_string()
    }

    async fn list(Query(filter): Query<Filter>) -> String {
        filter.owner
    }

    async fn search(ValidQuery(s): ValidQuery<Search>) -> String {
        s.q
    }

    fn app() -> Router {
        Router::new()
            .route("/items/:id", get(by_id))
            .route("/items", get(list))
            .route("/search", get(search))
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

    #[tokio::test]
    async fn valid_query_passes_a_satisfying_value() {
        let (status, _) = call("/search?q=hello").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn valid_query_rejects_a_constraint_failure_with_422() {
        // Structurally fine (`q` present), but fails the not-empty constraint.
        let (status, ct) = call("/search?q=").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(ct.as_deref(), Some(PROBLEM_CONTENT_TYPE));
    }

    #[tokio::test]
    async fn valid_query_rejects_a_missing_parameter_with_400() {
        // `q` absent → the bind itself fails (400), before validation runs.
        let (status, ct) = call("/search").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(ct.as_deref(), Some(PROBLEM_CONTENT_TYPE));
    }
}
