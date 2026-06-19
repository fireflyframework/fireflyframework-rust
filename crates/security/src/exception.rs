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

//! Pluggable rejection rendering — the Rust analog of Spring Security's
//! `ExceptionTranslationFilter` seam: an [`AuthenticationEntryPoint`] renders
//! the `401` for an unauthenticated request to a protected resource, and an
//! [`AccessDeniedHandler`] renders the `403` when an authenticated principal
//! lacks the required authority.
//!
//! [`FilterChain`](crate::FilterChain) uses these to render its rejections;
//! both default to the canonical RFC 7807 `application/problem+json` envelopes,
//! and either can be overridden (e.g. to redirect a browser to a login page, or
//! to add a `WWW-Authenticate` challenge).

use axum::extract::Request;
use axum::response::Response;
use http::header;

use crate::problem;

/// Renders the response when an **unauthenticated** request hits a protected
/// resource — Spring's `AuthenticationEntryPoint.commence`.
pub trait AuthenticationEntryPoint: Send + Sync {
    /// Builds the rejection response (typically `401`).
    fn commence(&self, request: &Request, detail: &str) -> Response;
}

/// Renders the response when an **authenticated** principal lacks the required
/// authority — Spring's `AccessDeniedHandler.handle`.
pub trait AccessDeniedHandler: Send + Sync {
    /// Builds the rejection response (typically `403`).
    fn handle(&self, request: &Request, detail: &str) -> Response;
}

/// The default [`AuthenticationEntryPoint`]: the canonical RFC 7807 `401`
/// `application/problem+json` envelope.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProblemAuthenticationEntryPoint;

impl AuthenticationEntryPoint for ProblemAuthenticationEntryPoint {
    fn commence(&self, _request: &Request, detail: &str) -> Response {
        problem::unauthorized(detail)
    }
}

/// The default [`AccessDeniedHandler`]: the canonical RFC 7807 `403`
/// `application/problem+json` envelope.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProblemAccessDeniedHandler;

impl AccessDeniedHandler for ProblemAccessDeniedHandler {
    fn handle(&self, _request: &Request, detail: &str) -> Response {
        problem::forbidden(detail)
    }
}

/// An [`AuthenticationEntryPoint`] that issues an HTTP Basic challenge — the
/// Rust analog of Spring's `BasicAuthenticationEntryPoint`. Renders the
/// canonical `401` plus `WWW-Authenticate: Basic realm="<realm>",
/// charset="UTF-8"`, prompting the browser/client for credentials.
#[derive(Debug, Clone)]
pub struct BasicAuthenticationEntryPoint {
    realm: String,
}

impl BasicAuthenticationEntryPoint {
    /// Builds the entry point for `realm`.
    #[must_use]
    pub fn new(realm: impl Into<String>) -> Self {
        Self {
            realm: realm.into(),
        }
    }
}

impl Default for BasicAuthenticationEntryPoint {
    fn default() -> Self {
        Self::new("Realm")
    }
}

impl AuthenticationEntryPoint for BasicAuthenticationEntryPoint {
    fn commence(&self, _request: &Request, detail: &str) -> Response {
        let mut response = problem::unauthorized(detail);
        let realm = sanitize_realm(&self.realm);
        let challenge = format!("Basic realm=\"{realm}\", charset=\"UTF-8\"");
        if let Ok(value) = http::HeaderValue::from_str(&challenge) {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, value);
        }
        response
    }
}

/// Strips characters that would break (or inject into) the `WWW-Authenticate`
/// quoted-string `realm`.
fn sanitize_realm(realm: &str) -> String {
    realm
        .chars()
        .filter(|c| *c != '"' && *c != '\\' && !c.is_control())
        .collect()
}
