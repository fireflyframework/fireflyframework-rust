//! Real Firebase Cloud Messaging (FCM v1) push delivery — the pyfly-parity
//! layer.
//!
//! Port of `pyfly.notifications.providers.firebase.FirebasePushProvider`. The
//! adapter posts once per device token to
//! `https://fcm.googleapis.com/v1/projects/{project_id}/messages:send` with an
//! `Authorization: Bearer <token>` header and a body of
//! `{"message": {"token", "notification": {"title","body"}, "data": {..}}}`.
//! `data` values are coerced to strings (matching pyfly's
//! `{k: str(v) for k, v in message.data.items()}`).
//!
//! ## Partial-success semantics
//!
//! Each token is sent independently. A 2xx appends the response `name` to the
//! delivered set; a non-2xx records `"{token}: http {status}"`. The aggregate
//! [`NotificationResult`]:
//!
//! * all delivered, no errors → [`DeliveryStatus::Sent`], `provider_id` is the
//!   `;`-joined message names, `error` is `None`;
//! * at least one delivered **and** some errors → [`DeliveryStatus::Sent`] with
//!   both `provider_id` and `error` populated (partial success);
//! * none delivered → [`DeliveryStatus::Failed`], `provider_id` is `None`.
//!
//! ## Access-token source
//!
//! FCM v1 requires a short-lived OAuth2 bearer token minted from a Google
//! service-account key. **This crate does not implement the service-account
//! JWT → OAuth2 exchange.** Instead it accepts an injected
//! [`AccessTokenProvider`] closure that yields the current bearer token on each
//! send, exactly as pyfly takes a pre-minted `access_token`. Wire it to
//! whatever mints/refreshes tokens in your deployment (e.g. the GCP metadata
//! server, a workload-identity sidecar, or `google-auth`-style libraries). For
//! a fixed token, use [`FirebasePushProvider::new`]; for a refreshing source,
//! use [`FirebasePushProvider::with_token_provider`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

/// The default FCM v1 base URL.
pub const DEFAULT_BASE_URL: &str = "https://fcm.googleapis.com";

/// Delivery status of a notification send attempt.
///
/// Port of pyfly's `EmailStatus` `StrEnum`, reused across e-mail/SMS/push
/// results. [`DeliveryStatus::as_str`] is wire-equal to pyfly's enum values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeliveryStatus {
    /// Queued for delivery (`"QUEUED"`).
    Queued,
    /// Accepted by the provider (`"SENT"`).
    Sent,
    /// Confirmed delivered (`"DELIVERED"`).
    Delivered,
    /// Bounced (`"BOUNCED"`).
    Bounced,
    /// Delivery failed (`"FAILED"`).
    Failed,
    /// Suppressed by an opt-out preference (`"SUPPRESSED"`).
    Suppressed,
}

impl DeliveryStatus {
    /// Returns the wire string, byte-equal to pyfly's `EmailStatus` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryStatus::Queued => "QUEUED",
            DeliveryStatus::Sent => "SENT",
            DeliveryStatus::Delivered => "DELIVERED",
            DeliveryStatus::Bounced => "BOUNCED",
            DeliveryStatus::Failed => "FAILED",
            DeliveryStatus::Suppressed => "SUPPRESSED",
        }
    }
}

/// A push message to deliver to one or more device tokens.
///
/// Port of pyfly's `PushMessage` dataclass. `id` defaults to a fresh UUID v4;
/// `data` is an arbitrary key/value map whose values are coerced to strings by
/// the FCM adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct PushMessage {
    /// Caller- or framework-assigned message id (defaults to a UUID v4).
    pub id: String,
    /// Target device registration tokens; one HTTP send per token.
    pub device_tokens: Vec<String>,
    /// Notification title.
    pub title: String,
    /// Notification body.
    pub body: String,
    /// Arbitrary data payload; values are stringified for the FCM `data` map.
    pub data: Map<String, Value>,
}

impl PushMessage {
    /// Builds a message for `device_tokens` with `title` and `body`, a fresh
    /// UUID id, and an empty data map.
    pub fn new(
        device_tokens: impl IntoIterator<Item = impl Into<String>>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        PushMessage {
            id: Uuid::new_v4().to_string(),
            device_tokens: device_tokens.into_iter().map(Into::into).collect(),
            title: title.into(),
            body: body.into(),
            data: Map::new(),
        }
    }

    /// Sets the data payload.
    pub fn with_data(mut self, data: Map<String, Value>) -> Self {
        self.data = data;
        self
    }
}

impl Default for PushMessage {
    fn default() -> Self {
        PushMessage {
            id: Uuid::new_v4().to_string(),
            device_tokens: Vec::new(),
            title: String::new(),
            body: String::new(),
            data: Map::new(),
        }
    }
}

/// The outcome of a push send (possibly spanning multiple tokens).
///
/// Port of pyfly's `NotificationResult` dataclass; see the module docs for the
/// partial-success rules that determine `status`, `provider_id`, and `error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationResult {
    /// The originating message id.
    pub id: String,
    /// The provider name that produced this result (`"firebase"`).
    pub provider: String,
    /// The aggregate delivery status.
    pub status: DeliveryStatus,
    /// `;`-joined delivered message names, when at least one token succeeded.
    pub provider_id: Option<String>,
    /// `; `-joined `"{token}: http {status}"` failures, when any token failed.
    pub error: Option<String>,
}

/// Errors raised by [`FirebasePushProvider::send`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FirebaseError {
    /// The HTTP request could not be performed (transport / connection error).
    #[error("firebase transport error: {0}")]
    Transport(String),
    /// The injected [`AccessTokenProvider`] failed to yield a bearer token.
    #[error("firebase access-token error: {0}")]
    Token(String),
}

/// Yields the current FCM bearer token.
///
/// This is the seam pyfly fills with a pre-minted `access_token`. Implement it
/// over whatever mints/refreshes tokens in your environment. The closure is
/// invoked once per [`FirebasePushProvider::send`] call (so a refreshing
/// implementation can return a freshly-minted token), and may fail with a
/// [`FirebaseError::Token`].
///
/// A blanket impl is provided for `Fn() -> Result<String, String>` closures, so
/// the common cases are a one-liner:
///
/// ```
/// use firefly_notifications_firebase::FirebasePushProvider;
///
/// // fixed token
/// let _ = FirebasePushProvider::new("proj", "ya29.token");
/// // refreshing token source
/// let _ = FirebasePushProvider::with_token_provider("proj", || Ok("ya29.fresh".to_string()));
/// ```
pub trait AccessTokenProvider: Send + Sync {
    /// Returns the current bearer token, or an error string on failure.
    fn token(&self) -> Result<String, String>;
}

impl<F> AccessTokenProvider for F
where
    F: Fn() -> Result<String, String> + Send + Sync,
{
    fn token(&self) -> Result<String, String> {
        self()
    }
}

/// The async push provider port.
///
/// Port of pyfly's `PushProvider` protocol: a named adapter that sends a
/// [`PushMessage`] and returns a [`NotificationResult`]. The provider folds
/// per-token HTTP non-2xx responses into the aggregate result and only errors
/// for transport or token-acquisition failures.
#[async_trait::async_trait]
pub trait PushProvider: Send + Sync {
    /// The provider name (e.g. `"firebase"`).
    fn name(&self) -> &str;

    /// Sends `message` to every device token and returns the aggregate result.
    ///
    /// # Errors
    ///
    /// Returns [`FirebaseError::Token`] when the access-token provider fails,
    /// or [`FirebaseError::Transport`] when an HTTP request itself fails.
    async fn send(&self, message: PushMessage) -> Result<NotificationResult, FirebaseError>;
}

/// Firebase Cloud Messaging push provider (FCM HTTP v1).
///
/// Port of pyfly's `FirebasePushProvider`. Construct with the GCP project id
/// and an access-token source; see the module docs for the token-source seam
/// and the partial-success semantics.
#[derive(Clone)]
pub struct FirebasePushProvider {
    project_id: String,
    token_provider: Arc<dyn AccessTokenProvider>,
    base_url: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for FirebasePushProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirebasePushProvider")
            .field("project_id", &self.project_id)
            .field("base_url", &self.base_url)
            .field("token_provider", &"<dyn AccessTokenProvider>")
            .finish()
    }
}

impl FirebasePushProvider {
    /// The provider name, matching pyfly's `name = "firebase"`.
    pub const NAME: &'static str = "firebase";

    /// Builds a provider with a fixed access token (the pyfly constructor
    /// shape — a pre-minted bearer token).
    pub fn new(project_id: impl Into<String>, access_token: impl Into<String>) -> Self {
        let token = access_token.into();
        Self::with_token_provider(project_id, move || Ok(token.clone()))
    }

    /// Builds a provider with a custom [`AccessTokenProvider`], invoked once per
    /// send so the token can refresh.
    pub fn with_token_provider(
        project_id: impl Into<String>,
        token_provider: impl AccessTokenProvider + 'static,
    ) -> Self {
        FirebasePushProvider {
            project_id: project_id.into(),
            token_provider: Arc::new(token_provider),
            base_url: DEFAULT_BASE_URL.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Overrides the FCM base URL (defaults to [`DEFAULT_BASE_URL`]).
    ///
    /// Behavior tests point this at an in-process axum mock; production callers
    /// never call it.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Injects a custom [`reqwest::Client`].
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// The `messages:send` endpoint URL for this project.
    fn send_url(&self) -> String {
        format!(
            "{}/v1/projects/{}/messages:send",
            self.base_url.trim_end_matches('/'),
            self.project_id,
        )
    }
}

#[async_trait::async_trait]
impl PushProvider for FirebasePushProvider {
    fn name(&self) -> &str {
        Self::NAME
    }

    async fn send(&self, message: PushMessage) -> Result<NotificationResult, FirebaseError> {
        let access_token = self.token_provider.token().map_err(FirebaseError::Token)?;
        let url = self.send_url();

        // Stringify the data map, matching pyfly's {k: str(v) ...}.
        let data: Map<String, Value> = message
            .data
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(stringify_value(v))))
            .collect();

        let mut sent_ids: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        for token in &message.device_tokens {
            let payload = serde_json::json!({
                "message": {
                    "token": token,
                    "notification": { "title": message.title, "body": message.body },
                    "data": data,
                }
            });

            let resp = self
                .http
                .post(&url)
                .bearer_auth(&access_token)
                .json(&payload)
                .send()
                .await
                .map_err(|e| FirebaseError::Transport(e.to_string()))?;

            let status = resp.status();
            if status.is_success() {
                let body: Value = resp
                    .json()
                    .await
                    .map_err(|e| FirebaseError::Transport(e.to_string()))?;
                let name = body
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                sent_ids.push(name);
            } else {
                errors.push(format!("{}: http {}", token, status.as_u16()));
            }
        }

        let provider_id = if sent_ids.is_empty() {
            None
        } else {
            Some(sent_ids.join(";"))
        };
        let error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        };
        let status = if !sent_ids.is_empty() {
            DeliveryStatus::Sent
        } else {
            DeliveryStatus::Failed
        };

        Ok(NotificationResult {
            id: message.id,
            provider: Self::NAME.to_string(),
            status,
            provider_id,
            error,
        })
    }
}

/// Renders a JSON value the way Python's `str()` would for FCM `data`:
/// strings pass through unquoted, everything else uses its JSON text.
fn stringify_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
