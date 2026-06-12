//! GraphQL client — POSTs a single operation to one endpoint over HTTP.
//!
//! The Rust port of pyfly's `GraphQLClient`: a thin `reqwest`-backed
//! client that POSTs `{ query, variables?, operationName? }` as JSON,
//! raises [`ClientError::GraphQl`] when the response carries a non-empty
//! `errors` array, and otherwise decodes the `data` field into the
//! caller's output type.
//!
//! Unlike pyfly — which returns a loosely-typed `dict` — the Rust
//! [`GraphQlClient::execute`] is generic over both the variables type
//! (`V: Serialize`) and the response type (`T: DeserializeOwned`), so
//! the `data` payload deserializes straight into a domain struct.

use std::time::Duration;

use http::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::de::DeserializeOwned;
use serde::Serialize;

use firefly_kernel::{
    correlation_id, FireflyError, ProblemDetail, HEADER_CORRELATION_ID, PROBLEM_CONTENT_TYPE,
};
use firefly_observability::inject_headers;

use crate::error::ClientError;

/// Default per-request timeout (30 s), matching pyfly's `GraphQLClient`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Fluently configures a [`GraphQlClient`] — the Rust analog of pyfly's
/// `GraphQLClientBuilder`.
///
/// ```
/// use firefly_client::GraphQlBuilder;
///
/// let client = GraphQlBuilder::new("https://api.example.com/graphql")
///     .with_header("Authorization", "Bearer token")
///     .with_timeout(std::time::Duration::from_secs(5))
///     .build();
/// # let _ = client;
/// ```
#[derive(Debug, Clone)]
pub struct GraphQlBuilder {
    endpoint: String,
    headers: HeaderMap,
    timeout: Duration,
    http_client: Option<reqwest::Client>,
}

impl GraphQlBuilder {
    /// Returns a builder primed for the given GraphQL endpoint.
    pub fn new(endpoint: impl AsRef<str>) -> Self {
        Self {
            endpoint: endpoint.as_ref().to_owned(),
            headers: HeaderMap::new(),
            timeout: DEFAULT_TIMEOUT,
            http_client: None,
        }
    }

    /// Sets a default header forwarded on every operation (pyfly's
    /// `with_header`), replacing any previous value for the same name.
    ///
    /// # Panics
    ///
    /// Panics when `key` is not a valid header name or `value` is not a
    /// valid header value — a programming error at wiring time.
    #[must_use]
    pub fn with_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = HeaderName::from_bytes(key.as_ref().as_bytes())
            .expect("GraphQlBuilder::with_header: invalid header name");
        let value = HeaderValue::from_str(value.as_ref())
            .expect("GraphQlBuilder::with_header: invalid header value");
        self.headers.insert(name, value);
        self
    }

    /// Overrides the per-request timeout (default 30 s, pyfly's default).
    /// Ignored when a custom client is injected via
    /// [`GraphQlBuilder::with_http_client`].
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Injects a custom [`reqwest::Client`], used as-is — convenient for
    /// sharing a pooled client or pointing tests at a mock transport.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Finalises the [`GraphQlClient`]. When no custom client was
    /// injected, a [`reqwest::Client`] is built with the configured
    /// timeout.
    pub fn build(self) -> GraphQlClient {
        let http = self.http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(self.timeout)
                .build()
                .expect("GraphQlBuilder::build: reqwest client construction failed")
        });
        GraphQlClient {
            endpoint: self.endpoint,
            headers: self.headers,
            http,
        }
    }
}

/// A thin GraphQL-over-HTTP client built by [`GraphQlBuilder`] — the
/// Rust analog of pyfly's `GraphQLClient`.
#[derive(Debug, Clone)]
pub struct GraphQlClient {
    endpoint: String,
    headers: HeaderMap,
    http: reqwest::Client,
}

impl GraphQlClient {
    /// Returns a builder primed for the given endpoint.
    pub fn builder(endpoint: impl AsRef<str>) -> GraphQlBuilder {
        GraphQlBuilder::new(endpoint)
    }

    /// Executes a GraphQL operation and decodes the `data` payload
    /// into `T`.
    ///
    /// The request body is `{ "query": query }` plus `"variables"` and
    /// `"operationName"` when supplied — matching pyfly's payload shape
    /// exactly, including omitting the optional keys when `None`. Use
    /// [`no_variables`] for the common no-variables case.
    ///
    /// # Errors
    ///
    /// * [`ClientError::GraphQl`] when the response carries a non-empty
    ///   `errors` array (the operation's `data`, if any, is discarded —
    ///   pyfly's behaviour).
    /// * [`ClientError::Problem`] for a non-2xx HTTP response, with an
    ///   RFC 7807 body decoded when present (the same shape every other
    ///   Firefly client surfaces).
    /// * [`ClientError::Transport`] / [`ClientError::Encode`] /
    ///   [`ClientError::Decode`] for transport, request-encode, and
    ///   response-decode failures respectively.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), firefly_client::ClientError> {
    /// use firefly_client::{no_variables, GraphQlBuilder};
    /// use serde::Deserialize;
    ///
    /// #[derive(Deserialize)]
    /// struct Data {
    ///     user: User,
    /// }
    /// #[derive(Deserialize)]
    /// struct User {
    ///     id: String,
    /// }
    ///
    /// let client = GraphQlBuilder::new("https://api.example.com/graphql").build();
    /// let data: Data = client
    ///     .execute("{ user { id } }", no_variables(), None)
    ///     .await?;
    /// assert!(!data.user.id.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn execute<V, T>(
        &self,
        query: &str,
        variables: Option<&V>,
        operation_name: Option<&str>,
    ) -> Result<T, ClientError>
    where
        V: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let mut body = serde_json::Map::new();
        body.insert(
            "query".to_owned(),
            serde_json::Value::String(query.to_owned()),
        );
        if let Some(vars) = variables {
            body.insert(
                "variables".to_owned(),
                serde_json::to_value(vars).map_err(ClientError::Encode)?,
            );
        }
        if let Some(op) = operation_name {
            body.insert(
                "operationName".to_owned(),
                serde_json::Value::String(op.to_owned()),
            );
        }
        let payload =
            serde_json::to_vec(&serde_json::Value::Object(body)).map_err(ClientError::Encode)?;

        let mut headers = self.headers.clone();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Some(id) = correlation_id() {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()),
                HeaderValue::from_str(&id),
            ) {
                headers.insert(name, value);
            }
        }
        inject_headers(&mut headers);

        let resp = self
            .http
            .post(&self.endpoint)
            .headers(headers)
            .body(payload)
            .send()
            .await
            .map_err(ClientError::Transport)?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let raw = resp.bytes().await.map_err(ClientError::Transport)?;

        if !status.is_success() {
            let mut ferr = FireflyError::new(
                String::new(),
                status.canonical_reason().unwrap_or_default(),
                status.as_u16(),
                String::from_utf8_lossy(&raw).into_owned(),
            );
            if content_type.starts_with(PROBLEM_CONTENT_TYPE) {
                if let Ok(pd) = serde_json::from_slice::<ProblemDetail>(&raw) {
                    ferr.code = pd.problem_type;
                    ferr.title = pd.title;
                    ferr.status = pd.status;
                    ferr.detail = pd.detail;
                    ferr.fields = pd.extensions;
                }
            }
            return Err(ClientError::Problem(ferr));
        }

        let envelope: GraphQlResponse =
            serde_json::from_slice(&raw).map_err(ClientError::Decode)?;
        if let Some(errors) = envelope.errors {
            if !errors.is_empty() {
                return Err(ClientError::GraphQl(errors));
            }
        }
        // pyfly maps a missing/null `data` to `{}`; the Rust port lets
        // the caller's `T` decide — `null` round-trips to `T = ()` /
        // `Option<_>`, and an absent key decodes the JSON `null`.
        let data = envelope.data.unwrap_or(serde_json::Value::Null);
        serde_json::from_value(data).map_err(ClientError::Decode)
    }
}

/// The standard GraphQL response envelope (`{ data?, errors? }`).
#[derive(serde::Deserialize)]
struct GraphQlResponse {
    #[serde(default)]
    data: Option<serde_json::Value>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

/// Spells the body-less / variables-less turbofish for
/// [`GraphQlClient::execute`], so call sites read
/// `client.execute(query, no_variables(), None)` instead of
/// `client.execute::<(), _>(query, None, None)`.
#[must_use]
pub const fn no_variables() -> Option<&'static serde_json::Value> {
    None
}
