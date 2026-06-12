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

//! Real Twilio SMS delivery — the pyfly-parity layer.
//!
//! Port of `pyfly.notifications.providers.twilio.TwilioSmsProvider`. The
//! adapter posts to Twilio's REST endpoint
//! `https://api.twilio.com/2010-04-01/Accounts/{sid}/Messages.json` with HTTP
//! basic auth (`account_sid` / `auth_token`) and a form-encoded body
//! (`From` / `To` / `Body`). On a 2xx it parses the JSON `sid` into the result
//! `provider_id`; any non-2xx maps to a [`DeliveryStatus::Failed`] result
//! carrying `http {status}: {body}`.
//!
//! Unlike pyfly (which hardcodes the URL and injects a fake `httpx` client for
//! tests), this Rust port exposes a configurable base URL via
//! [`TwilioSmsProvider::with_base_url`] so behavior tests can point the adapter
//! at an in-process axum mock. The default base URL is the real Twilio host, so
//! production callers never need to set it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The default Twilio REST API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.twilio.com";

/// Delivery status of a notification send attempt.
///
/// Port of pyfly's `EmailStatus` `StrEnum` — the notifications module reuses
/// the same status enum across e-mail, SMS, and push results. The string
/// representation ([`DeliveryStatus::as_str`]) is wire-equal to pyfly's enum
/// values (`"SENT"`, `"FAILED"`, …).
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

/// An SMS message to deliver.
///
/// Port of pyfly's `SmsMessage` dataclass. `id` defaults to a fresh UUID v4
/// (matching `field(default_factory=lambda: str(uuid.uuid4()))`); `sender` is
/// optional and, when set, takes precedence over the provider's configured
/// `from_number`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsMessage {
    /// Caller- or framework-assigned message id (defaults to a UUID v4).
    pub id: String,
    /// Destination phone number in E.164 form.
    pub to: String,
    /// Message text.
    pub body: String,
    /// Optional per-message sender; wins over the provider `from_number`.
    pub sender: Option<String>,
}

impl SmsMessage {
    /// Builds a message to `to` with `body`, a fresh UUID id, and no sender.
    pub fn new(to: impl Into<String>, body: impl Into<String>) -> Self {
        SmsMessage {
            id: Uuid::new_v4().to_string(),
            to: to.into(),
            body: body.into(),
            sender: None,
        }
    }

    /// Sets the per-message sender (wins over the provider `from_number`).
    pub fn with_sender(mut self, sender: impl Into<String>) -> Self {
        self.sender = Some(sender.into());
        self
    }
}

impl Default for SmsMessage {
    fn default() -> Self {
        SmsMessage {
            id: Uuid::new_v4().to_string(),
            to: String::new(),
            body: String::new(),
            sender: None,
        }
    }
}

/// The outcome of a single provider send.
///
/// Port of pyfly's `NotificationResult` dataclass. On success `status` is
/// [`DeliveryStatus::Sent`] and `provider_id` carries the provider's message id
/// (Twilio's `sid`); on failure `status` is [`DeliveryStatus::Failed`] and
/// `error` carries the rendered cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationResult {
    /// The originating message id.
    pub id: String,
    /// The provider name that produced this result (`"twilio"`).
    pub provider: String,
    /// The delivery status.
    pub status: DeliveryStatus,
    /// The provider-side message id, when the send succeeded.
    pub provider_id: Option<String>,
    /// The rendered error, when the send failed.
    pub error: Option<String>,
}

/// The lifecycle status of a previously-sent Twilio message, as reported by the
/// [Message resource `status` field](https://www.twilio.com/docs/sms/api/message-resource#message-status-values).
///
/// Returned by [`TwilioSmsProvider::fetch_status`]. The [`MessageStatus::status`]
/// field carries the exact lower-case value Twilio returns (`"queued"`,
/// `"sent"`, `"delivered"`, `"failed"`, `"undelivered"`, …) verbatim, so
/// callers can match on any present or future status value without this crate
/// needing an exhaustive enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageStatus {
    /// The message SID this status describes (Twilio's `sid`).
    pub sid: String,
    /// The raw Twilio `status` string, verbatim from the response.
    pub status: String,
    /// Twilio's numeric `error_code`, present when the message failed.
    pub error_code: Option<i64>,
    /// Twilio's human-readable `error_message`, present when the message failed.
    pub error_message: Option<String>,
}

/// Errors raised by [`TwilioSmsProvider`] before or during an HTTP call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TwilioError {
    /// Neither the message `sender` nor the provider `from_number` was set.
    ///
    /// Message-equal to pyfly's `ValueError` ("…needs a sender…").
    #[error(
        "TwilioSmsProvider needs a sender — set sender on the message or from_number on the provider"
    )]
    MissingSender,
    /// The HTTP request could not be performed (transport / connection error).
    #[error("twilio transport error: {0}")]
    Transport(String),
    /// Twilio returned a non-2xx status for a status-fetch request.
    ///
    /// Carries the HTTP status code and the (possibly empty) response body, so
    /// callers can distinguish a missing SID (`404`) from an auth failure
    /// (`401`).
    #[error("twilio status fetch failed: http {status}: {body}")]
    StatusFetch {
        /// The HTTP status code Twilio returned.
        status: u16,
        /// The raw response body (typically a Twilio error JSON document).
        body: String,
    },
}

/// The async SMS provider port.
///
/// Port of pyfly's `SmsProvider` protocol: a named adapter that sends an
/// [`SmsMessage`] and returns a [`NotificationResult`]. The provider folds
/// HTTP non-2xx responses into a [`DeliveryStatus::Failed`] result (it does not
/// error); it only errors for pre-flight problems such as a missing sender.
#[async_trait::async_trait]
pub trait SmsProvider: Send + Sync {
    /// The provider name (e.g. `"twilio"`).
    fn name(&self) -> &str;

    /// Sends `message` and returns the delivery result.
    ///
    /// # Errors
    ///
    /// Returns [`TwilioError::MissingSender`] when no sender can be resolved,
    /// or [`TwilioError::Transport`] when the HTTP request itself fails.
    async fn send(&self, message: SmsMessage) -> Result<NotificationResult, TwilioError>;
}

/// Twilio SMS provider — posts to `Messages.json` with HTTP basic auth.
///
/// Port of pyfly's `TwilioSmsProvider`. Construct with the account SID, auth
/// token, and (optionally) a default `from_number`; per-message senders win
/// over the provider default.
///
/// ```
/// use firefly_notifications_twilio::{SmsProvider, SmsMessage, TwilioSmsProvider};
///
/// let provider = TwilioSmsProvider::new("AC_sid", "tok")
///     .with_from_number("+15550001111");
/// assert_eq!(provider.name(), "twilio");
/// # let _ = SmsMessage::new("+15559876543", "hi");
/// ```
#[derive(Debug, Clone)]
pub struct TwilioSmsProvider {
    account_sid: String,
    auth_token: String,
    from_number: Option<String>,
    base_url: String,
    http: reqwest::Client,
}

impl TwilioSmsProvider {
    /// The provider name, matching pyfly's `name = "twilio"`.
    pub const NAME: &'static str = "twilio";

    /// Builds a provider with the given account SID and auth token.
    pub fn new(account_sid: impl Into<String>, auth_token: impl Into<String>) -> Self {
        TwilioSmsProvider {
            account_sid: account_sid.into(),
            auth_token: auth_token.into(),
            from_number: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Sets the default sender number used when a message has no `sender`.
    pub fn with_from_number(mut self, from_number: impl Into<String>) -> Self {
        self.from_number = Some(from_number.into());
        self
    }

    /// Overrides the REST API base URL (defaults to [`DEFAULT_BASE_URL`]).
    ///
    /// Behavior tests point this at an in-process axum mock; production callers
    /// never call it.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Injects a custom [`reqwest::Client`] (e.g. with a timeout or proxy).
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// The endpoint URL for this account's `Messages.json` resource.
    fn messages_url(&self) -> String {
        format!(
            "{}/2010-04-01/Accounts/{}/Messages.json",
            self.base_url.trim_end_matches('/'),
            self.account_sid,
        )
    }

    /// The endpoint URL for a single message resource's status
    /// (`…/Messages/{sid}.json`).
    fn message_status_url(&self, sid: &str) -> String {
        format!(
            "{}/2010-04-01/Accounts/{}/Messages/{}.json",
            self.base_url.trim_end_matches('/'),
            self.account_sid,
            sid,
        )
    }

    /// Fetches the current delivery status of a previously-sent message by SID.
    ///
    /// Performs a `GET` against Twilio's
    /// [Message resource](https://www.twilio.com/docs/sms/api/message-resource)
    /// endpoint `…/2010-04-01/Accounts/{sid}/Messages/{message_sid}.json` with
    /// HTTP basic auth, and parses the `status`, `error_code`, and
    /// `error_message` fields into a [`MessageStatus`].
    ///
    /// Twilio's message status is the canonical source of delivery state after
    /// the initial accept-for-delivery response; this is how callers poll for
    /// `delivered` / `failed` / `undelivered` transitions when a status-callback
    /// webhook is not wired.
    ///
    /// # Errors
    ///
    /// Returns [`TwilioError::Transport`] if the request cannot be performed,
    /// or [`TwilioError::StatusFetch`] carrying the HTTP code and body for any
    /// non-2xx response (e.g. `404` for an unknown SID, `401` for bad auth).
    pub async fn fetch_status(&self, message_sid: &str) -> Result<MessageStatus, TwilioError> {
        let resp = self
            .http
            .get(self.message_status_url(message_sid))
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .send()
            .await
            .map_err(|e| TwilioError::Transport(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(TwilioError::StatusFetch {
                status: status.as_u16(),
                body,
            });
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| TwilioError::Transport(e.to_string()))?;

        Ok(MessageStatus {
            sid: body
                .get("sid")
                .and_then(|v| v.as_str())
                .unwrap_or(message_sid)
                .to_string(),
            status: body
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            error_code: body.get("error_code").and_then(serde_json::Value::as_i64),
            error_message: body
                .get("error_message")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }
}

#[async_trait::async_trait]
impl SmsProvider for TwilioSmsProvider {
    fn name(&self) -> &str {
        Self::NAME
    }

    async fn send(&self, message: SmsMessage) -> Result<NotificationResult, TwilioError> {
        // Sender precedence: message.sender wins over the provider default.
        let from_number = message
            .sender
            .clone()
            .or_else(|| self.from_number.clone())
            .ok_or(TwilioError::MissingSender)?;

        let form = [
            ("From", from_number.as_str()),
            ("To", message.to.as_str()),
            ("Body", message.body.as_str()),
        ];

        let resp = self
            .http
            .post(self.messages_url())
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&form)
            .send()
            .await
            .map_err(|e| TwilioError::Transport(e.to_string()))?;

        let status = resp.status();
        if status.is_success() {
            // Twilio returns the message resource; we want its `sid`.
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| TwilioError::Transport(e.to_string()))?;
            let provider_id = body.get("sid").and_then(|v| v.as_str()).map(str::to_string);
            Ok(NotificationResult {
                id: message.id,
                provider: Self::NAME.to_string(),
                status: DeliveryStatus::Sent,
                provider_id,
                error: None,
            })
        } else {
            let code = status.as_u16();
            let text = resp.text().await.unwrap_or_default();
            Ok(NotificationResult {
                id: message.id,
                provider: Self::NAME.to_string(),
                status: DeliveryStatus::Failed,
                provider_id: None,
                error: Some(format!("http {code}: {text}")),
            })
        }
    }
}
