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

//! Remote `/actuator/*` introspection client.
//!
//! Port of pyfly's `_introspect.py` *remote half* only. A compiled Rust binary
//! has no meaningful "offline context boot" (pyfly imports the app module and
//! starts its DI container in-process); the brief therefore documents `--url`
//! as the sole mode for `firefly actuator`.
//!
//! The workspace `reqwest` is built without the `blocking` feature, so this
//! client drives the async client on a small current-thread tokio runtime via
//! `block_on` — keeping the CLI handlers synchronous.

use std::time::Duration;

use crate::error::CliError;

/// Minimal synchronous client for a running app's `/actuator/*` endpoints.
///
/// Mirrors pyfly's `ActuatorClient`: it GETs `<base>/actuator/<endpoint>` and
/// returns the parsed JSON body.
#[derive(Debug, Clone)]
pub struct ActuatorClient {
    base: String,
    timeout: Duration,
}

impl ActuatorClient {
    /// Construct a client for `base_url` (trailing slash trimmed), with the
    /// default 10-second timeout.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base: base_url.into().trim_end_matches('/').to_string(),
            timeout: Duration::from_secs(10),
        }
    }

    /// Override the request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The full URL this client would GET for `endpoint`.
    pub fn url_for(&self, endpoint: &str) -> String {
        format!(
            "{}/actuator/{}",
            self.base,
            endpoint.trim_start_matches('/')
        )
    }

    /// GET `<base>/actuator/<endpoint>` and parse the JSON response.
    ///
    /// # Errors
    /// Returns [`CliError::Request`] on a transport error, a non-success HTTP
    /// status, or a malformed JSON body.
    pub fn get(&self, endpoint: &str) -> Result<serde_json::Value, CliError> {
        let url = self.url_for(endpoint);
        let timeout = self.timeout;
        let url_for_err = url.clone();
        block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .map_err(|e| CliError::Request {
                    url: url_for_err.clone(),
                    message: e.to_string(),
                })?;
            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| CliError::Request {
                    url: url_for_err.clone(),
                    message: e.to_string(),
                })?;
            let resp = resp.error_for_status().map_err(|e| CliError::Request {
                url: url_for_err.clone(),
                message: e.to_string(),
            })?;
            resp.json::<serde_json::Value>()
                .await
                .map_err(|e| CliError::Request {
                    url: url_for_err.clone(),
                    message: e.to_string(),
                })
        })
    }
}

/// Drive `fut` to completion on a fresh current-thread tokio runtime.
///
/// Used instead of `reqwest`'s `blocking` feature (absent from the workspace
/// catalog) so the CLI's command handlers can stay synchronous.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread tokio runtime")
        .block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_for_trims_and_joins() {
        let c = ActuatorClient::new("http://host:8080/");
        assert_eq!(c.url_for("health"), "http://host:8080/actuator/health");
        assert_eq!(c.url_for("/metrics"), "http://host:8080/actuator/metrics");
        assert_eq!(
            c.url_for("metrics/jvm"),
            "http://host:8080/actuator/metrics/jvm"
        );
    }

    #[test]
    fn get_against_in_process_server() {
        // Spin a one-shot in-process axum server on port 0, hit it via the
        // blocking client, and assert the JSON round-trips. No external server.
        use axum::{routing::get, Json, Router};
        use std::net::SocketAddr;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
        let handle = rt.spawn(async move {
            let app = Router::new().route(
                "/actuator/health",
                get(|| async { Json(serde_json::json!({ "status": "UP" })) }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        let addr = addr_rx.recv().unwrap();

        let client =
            ActuatorClient::new(format!("http://{addr}")).with_timeout(Duration::from_millis(500));
        let body = client.get("health").unwrap();
        assert_eq!(body["status"], "UP");

        handle.abort();
    }

    #[test]
    fn get_error_status_is_request_error() {
        use axum::{http::StatusCode, routing::get, Router};
        use std::net::SocketAddr;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
        let handle = rt.spawn(async move {
            let app =
                Router::new().route("/actuator/missing", get(|| async { StatusCode::NOT_FOUND }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        let addr = addr_rx.recv().unwrap();

        let client =
            ActuatorClient::new(format!("http://{addr}")).with_timeout(Duration::from_millis(500));
        let err = client.get("missing");
        assert!(matches!(err, Err(CliError::Request { .. })));

        handle.abort();
    }
}
