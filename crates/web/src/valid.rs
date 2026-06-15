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

//! The [`Valid<T>`] auto-validating JSON extractor — the Rust analog of a
//! Spring `@Valid @RequestBody` parameter.
//!
//! `Valid<T>` deserializes the request body as JSON into `T` (which must
//! be [`DeserializeOwned`] + [`Validate`]), then runs `T::validate()`.
//! A malformed body rejects with a 400 problem; a structurally-valid body
//! that fails a constraint rejects with a 422
//! `application/problem+json` carrying the structured per-field violations
//! (see [`firefly_validators::ValidationErrors`]'s `FireflyError`
//! conversion). On success the validated `T` is handed to the handler.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Json, Request};
use axum::response::{IntoResponse, Response};
use firefly_kernel::FireflyError;
use firefly_validators::bean::Validate;
use serde::de::DeserializeOwned;

use crate::problem::WebError;

/// An axum extractor that deserializes a JSON body into `T` and then runs
/// the type's declarative [`Validate`] constraints, rejecting the request
/// with an RFC 9457 problem before the handler runs if either step fails.
///
/// Use it exactly like [`axum::Json`], swapping `Json(payload)` for
/// `Valid(payload)`; the inner `T` is the validated value.
///
/// ```ignore
/// use firefly::prelude::*;          // Valid, Validate, WebResult
/// use serde::Deserialize;
///
/// #[derive(Deserialize, Validate)]
/// struct CreateUser {
///     #[validate(not_empty)]
///     name: String,
///     #[validate(email)]
///     email: String,
/// }
///
/// async fn create(Valid(user): Valid<CreateUser>) -> WebResult<&'static str> {
///     // `user` is guaranteed to satisfy every #[validate(...)] constraint.
///     Ok("created")
/// }
/// ```
///
/// Rejections:
/// - body is not valid JSON / does not decode into `T` → **400 Bad Request**
///   (the JSON rejection's message, as `application/problem+json`);
/// - body decodes but a constraint fails → **422 Validation Failed** with the
///   `{field, code, message}` violation list under the problem's `errors`
///   member.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Valid<T>(pub T);

impl<T> Valid<T> {
    /// Consumes the wrapper, returning the validated value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[axum::async_trait]
impl<T, S> FromRequest<S> for Valid<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Structural decode first — a JSON/shape error is a 400, mirroring
        // axum::Json. We re-render the rejection as an RFC 9457 problem so the
        // whole error surface stays `application/problem+json`.
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|rejection| {
                // Well-formed JSON whose shape/types do not match `T` is a 422
                // (unprocessable, like a constraint failure); a JSON syntax or
                // transport error is a 400 — axum's native split, preserved
                // through the RFC 9457 problem.
                let err = match rejection {
                    JsonRejection::JsonDataError(e) => FireflyError::validation(e.body_text()),
                    other => FireflyError::bad_request(other.body_text()),
                };
                WebError::from(err).into_response()
            })?;

        // Then the declarative constraints — a failure is a 422 carrying the
        // structured per-field violations.
        match value.validate() {
            Ok(()) => Ok(Valid(value)),
            Err(errors) => Err(WebError::from(FireflyError::from(errors)).into_response()),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::routing::post;
    use axum::Router;
    use firefly_kernel::PROBLEM_CONTENT_TYPE;
    use firefly_validators::bean::{Validate, ValidationError, ValidationErrors};
    use http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use tower::ServiceExt;

    use super::Valid;

    #[derive(Deserialize)]
    struct CreateUser {
        name: String,
        email: String,
    }

    // Hand-written Validate so this test does not depend on firefly-macros
    // (which would pull the heavier dev-build); the derive is tested there.
    impl Validate for CreateUser {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            if self.name.trim().is_empty() {
                errors.push(ValidationError::new(
                    "name",
                    "not_empty",
                    "must not be empty",
                ));
            }
            if !self.email.contains('@') {
                errors.push(ValidationError::new("email", "email", "not a valid email"));
            }
            errors.into_result()
        }
    }

    async fn handler(Valid(user): Valid<CreateUser>) -> String {
        format!("{}:{}", user.name, user.email)
    }

    fn app() -> Router {
        Router::new().route("/users", post(handler))
    }

    async fn post_json(body: &str) -> (StatusCode, Option<String>, String) {
        let res = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/users")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let content_type = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            content_type,
            String::from_utf8(bytes.to_vec()).unwrap(),
        )
    }

    #[tokio::test]
    async fn valid_body_reaches_the_handler() {
        let (status, _ct, body) = post_json(r#"{"name":"ada","email":"ada@example.com"}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ada:ada@example.com");
    }

    #[tokio::test]
    async fn invalid_body_rejects_with_422_problem() {
        let (status, content_type, body) = post_json(r#"{"name":"","email":"nope"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_CONTENT_TYPE));
        // Every failing field appears in the structured `errors` member.
        assert!(body.contains("\"errors\""), "body was: {body}");
        assert!(body.contains("not_empty"), "body was: {body}");
        assert!(body.contains("\"email\""), "body was: {body}");
        assert!(
            body.contains("https://fireflyframework.org/problems/validation"),
            "body was: {body}"
        );
    }

    #[tokio::test]
    async fn malformed_json_rejects_with_400_problem() {
        let (status, content_type, _body) = post_json("not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_CONTENT_TYPE));
    }
}
