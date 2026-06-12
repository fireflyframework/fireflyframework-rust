//! Typed admin client — the Rust counterpart of the Go
//! `ruleengine/sdk` package (planned for v26.06 in Go; implemented
//! here).
//!
//! [`RuleEngineClient`] maps one-for-one onto the [`crate::web`]
//! router: [`RuleEngineClient::evaluate`] posts an inline AST to
//! `/api/rules/evaluate`, [`RuleEngineClient::evaluate_yaml`] posts a
//! YAML document to `/api/rules/evaluate/yaml`, and both decode the
//! JSON [`Verdict`].
//!
//! HTTP is performed through the [`HttpTransport`] port so tests can
//! drive the client against an in-process [`axum::Router`] (via
//! `tower::ServiceExt::oneshot`) without opening sockets; the default
//! transport is [`ReqwestTransport`].
//!
//! # Quick start
//!
//! ```rust,no_run
//! use firefly_rule_engine::{RuleEngineClient, RuleSet};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let client = RuleEngineClient::new("http://rules.internal:8080");
//! let set = RuleSet::from_yaml("name: demo\nrules: []")?;
//! let verdict = client.evaluate(&set, &serde_json::Map::new()).await?;
//! assert!(verdict.matched.is_empty());
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::interfaces::{Fact, Verdict};
use crate::models::RuleSet;
use crate::web::ErrorBody;

/// Errors surfaced by [`RuleEngineClient`].
#[derive(Debug, Error)]
pub enum SdkError {
    /// The request never produced an HTTP response (connection refused,
    /// DNS failure, …).
    #[error("ruleengine sdk: transport: {0}")]
    Transport(String),
    /// The server answered with a non-2xx status; `message` carries the
    /// `error` member of the JSON body when present, the raw body
    /// otherwise.
    #[error("ruleengine sdk: http {status}: {message}")]
    Http {
        /// HTTP status code of the response.
        status: u16,
        /// Server-provided error description.
        message: String,
    },
    /// The response body was not a valid JSON [`Verdict`].
    #[error("ruleengine sdk: decode: {0}")]
    Decode(String),
}

/// Minimal HTTP port the client speaks through — lets tests exercise
/// the full client ↔ router contract in-process.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// POSTs `body` as `application/json` to the absolute `url` and
    /// returns the response status and raw body bytes.
    async fn post_json(&self, url: &str, body: &Value) -> Result<(u16, Vec<u8>), SdkError>;
}

/// Production [`HttpTransport`] backed by [`reqwest::Client`].
#[derive(Debug, Clone, Default)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Builds a transport with a default [`reqwest::Client`].
    pub fn new() -> Self {
        ReqwestTransport::default()
    }

    /// Builds a transport around a pre-configured client (timeouts,
    /// proxies, TLS, …).
    pub fn with_client(client: reqwest::Client) -> Self {
        ReqwestTransport { client }
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn post_json(&self, url: &str, body: &Value) -> Result<(u16, Vec<u8>), SdkError> {
        let response = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|e| SdkError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| SdkError::Transport(e.to_string()))?;
        Ok((status, bytes.to_vec()))
    }
}

/// Typed client for the rule-engine REST API exposed by
/// [`crate::web::rule_engine_router`]. All methods map one-for-one
/// onto the router's endpoints.
#[derive(Clone)]
pub struct RuleEngineClient {
    base_url: String,
    transport: Arc<dyn HttpTransport>,
}

impl RuleEngineClient {
    /// Builds a client for the service at `base_url`
    /// (e.g. `http://rules.internal:8080`), using the default
    /// [`ReqwestTransport`]. A trailing slash on the base URL is
    /// tolerated.
    pub fn new(base_url: impl Into<String>) -> Self {
        RuleEngineClient::with_transport(base_url, Arc::new(ReqwestTransport::new()))
    }

    /// Builds a client with a custom [`HttpTransport`] — the seam used
    /// by the in-process tests.
    pub fn with_transport(base_url: impl Into<String>, transport: Arc<dyn HttpTransport>) -> Self {
        RuleEngineClient {
            base_url: base_url.into(),
            transport,
        }
    }

    /// `POST /api/rules/evaluate` — evaluates an inline AST rule set
    /// against `fact`.
    pub async fn evaluate(&self, set: &RuleSet, fact: &Fact) -> Result<Verdict, SdkError> {
        self.post(
            "/api/rules/evaluate",
            serde_json::json!({"ruleset": set, "fact": fact}),
        )
        .await
    }

    /// `POST /api/rules/evaluate/yaml` — evaluates a YAML rule
    /// document against `fact`.
    pub async fn evaluate_yaml(&self, yaml: &str, fact: &Fact) -> Result<Verdict, SdkError> {
        self.post(
            "/api/rules/evaluate/yaml",
            serde_json::json!({"yaml": yaml, "fact": fact}),
        )
        .await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Verdict, SdkError> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let (status, bytes) = self.transport.post_json(&url, &body).await?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_slice::<ErrorBody>(&bytes)
                .map(|b| b.error)
                .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
            return Err(SdkError::Http { status, message });
        }
        serde_json::from_slice(&bytes).map_err(|e| SdkError::Decode(e.to_string()))
    }
}
