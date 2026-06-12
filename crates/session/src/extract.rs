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

//! [`SessionExt`] — an axum extractor for the request's [`Session`].
//!
//! The [`crate::SessionLayer`] inserts a [`Session`] handle into the request
//! extensions. Handlers can pull it out with either `axum::Extension<Session>`
//! or this newtype extractor, which yields a clearer error
//! (`500 SESSION_LAYER_MISSING`) when the layer is not installed.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::session::Session;

/// Extracts the request's [`Session`] handle (inserted by
/// [`crate::SessionLayer`]). Deref-transparent to [`Session`].
///
/// ```ignore
/// async fn handler(SessionExt(session): SessionExt) {
///     session.set_attribute("user", "ada").await.unwrap();
/// }
/// ```
#[derive(Debug, Clone)]
pub struct SessionExt(pub Session);

impl std::ops::Deref for SessionExt {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Returned by the [`SessionExt`] extractor when no [`Session`] is present
/// in the request extensions — i.e. the [`crate::SessionLayer`] was not
/// installed ahead of the handler.
#[derive(Debug, Clone, Copy)]
pub struct SessionLayerMissing;

impl IntoResponse for SessionLayerMissing {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "firefly/session: SessionLayer is not installed",
        )
            .into_response()
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for SessionExt {
    type Rejection = SessionLayerMissing;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Session>()
            .cloned()
            .map(SessionExt)
            .ok_or(SessionLayerMissing)
    }
}
