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

//! HTTP endpoints for the [`AuthorizationServer`] — turns the callable token
//! logic into a mountable OAuth2 surface, plus RFC 8414 discovery.
//!
//! [`AuthorizationServerRouter::router`] mounts:
//!
//! * `POST /oauth2/token` — the RFC 6749 token endpoint (the `client_credentials`
//!   and `refresh_token` grants the [`AuthorizationServer`] supports), with
//!   client authentication via `client_secret_post` (form fields). Success is a
//!   `200` [`TokenResponse`]; a failure is the RFC 6749 §5.2 error envelope
//!   (`{"error", "error_description"}`, `401` for `invalid_client`, else `400`).
//! * `GET /.well-known/oauth-authorization-server` — the RFC 8414 Authorization
//!   Server Metadata document (issuer, token endpoint, supported grants and
//!   client-auth methods). No `jwks_uri` is advertised: tokens are HS256-signed,
//!   so there is no public verification key to publish.

use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http::StatusCode;
use serde_json::{json, Value};

use super::authorization_server::{AuthorizationServer, TokenRequest};

/// Shared state behind the authorization-server endpoints.
struct EndpointsState {
    server: Arc<AuthorizationServer>,
    issuer: String,
}

/// Mounts the [`AuthorizationServer`] as HTTP endpoints — the token endpoint and
/// RFC 8414 metadata.
pub struct AuthorizationServerRouter {
    state: Arc<EndpointsState>,
}

impl AuthorizationServerRouter {
    /// Builds the router over `server`, advertising `issuer` (the server's public
    /// base URL, e.g. `https://auth.example.com`) in the metadata + endpoint URLs.
    #[must_use]
    pub fn new(server: Arc<AuthorizationServer>, issuer: impl Into<String>) -> Self {
        Self {
            state: Arc::new(EndpointsState {
                server,
                issuer: issuer.into(),
            }),
        }
    }

    /// The axum [`Router`] mounting `POST /oauth2/token` and
    /// `GET /.well-known/oauth-authorization-server`.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/oauth2/token", post(token_endpoint))
            .route(
                "/.well-known/oauth-authorization-server",
                get(metadata_endpoint),
            )
            .with_state(Arc::clone(&self.state))
    }
}

/// `POST /oauth2/token` — RFC 6749 token endpoint.
async fn token_endpoint(
    State(state): State<Arc<EndpointsState>>,
    Form(request): Form<TokenRequest>,
) -> Response {
    match state.server.token(&request).await {
        Ok(token) => (StatusCode::OK, Json(token)).into_response(),
        Err(error) => {
            if error.code.eq_ignore_ascii_case("server_error") {
                // An internal failure (e.g. token signing) is a 5xx; never echo
                // the raw internal error detail to the caller.
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": "server_error",
                        "error_description": "internal error",
                    })),
                )
                    .into_response();
            }
            // RFC 6749 §5.2: `invalid_client` is a `401`; other token errors a
            // `400`. Error codes are lowercased to the registered RFC names.
            let status = if error.code.eq_ignore_ascii_case("invalid_client") {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_REQUEST
            };
            let body = json!({
                "error": error.code.to_lowercase(),
                "error_description": error.message,
            });
            (status, Json(body)).into_response()
        }
    }
}

/// `GET /.well-known/oauth-authorization-server` — RFC 8414 metadata.
async fn metadata_endpoint(State(state): State<Arc<EndpointsState>>) -> Json<Value> {
    let issuer = state.issuer.trim_end_matches('/');
    Json(json!({
        "issuer": issuer,
        "token_endpoint": format!("{issuer}/oauth2/token"),
        "grant_types_supported": ["client_credentials", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["client_secret_post"],
        // No authorization_code/PKCE server flow yet, so no authorization or
        // response types are advertised.
        "response_types_supported": [],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth2::{
        ClientRegistration, InMemoryClientRegistrationRepository, InMemoryTokenStore,
    };
    use http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn router() -> Router {
        let repo =
            InMemoryClientRegistrationRepository::new([ClientRegistration::new("m2m", "m2m")
                .client_secret("s3cret")
                .authorization_grant_type("client_credentials")
                .scopes(&["api"])]);
        let server = Arc::new(AuthorizationServer::new(
            "signing-secret",
            Arc::new(repo),
            Arc::new(InMemoryTokenStore::new()),
        ));
        AuthorizationServerRouter::new(server, "https://auth.example.com/").router()
    }

    async fn body_json(resp: Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn form_post(uri: &str, body: &str) -> Request<axum::body::Body> {
        Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .header(
                http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn token_endpoint_issues_for_client_credentials() {
        let resp = router()
            .oneshot(form_post(
                "/oauth2/token",
                "grant_type=client_credentials&client_id=m2m&client_secret=s3cret",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body.get("access_token").and_then(Value::as_str).is_some());
        assert_eq!(body["token_type"], "Bearer");
        assert_eq!(body["scope"], "api");
    }

    #[tokio::test]
    async fn token_endpoint_rejects_bad_client_with_401() {
        let resp = router()
            .oneshot(form_post(
                "/oauth2/token",
                "grant_type=client_credentials&client_id=m2m&client_secret=wrong",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(resp).await["error"], "invalid_client");
    }

    #[tokio::test]
    async fn metadata_document_advertises_issuer_and_token_endpoint() {
        let resp = router()
            .oneshot(
                Request::builder()
                    .uri("/.well-known/oauth-authorization-server")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["issuer"], "https://auth.example.com");
        assert_eq!(
            body["token_endpoint"],
            "https://auth.example.com/oauth2/token"
        );
        assert_eq!(body["grant_types_supported"][0], "client_credentials");
    }
}
