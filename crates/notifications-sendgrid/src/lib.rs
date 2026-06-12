//! firefly-notifications-sendgrid — the SendGrid e-mail adapter.
//!
//! This crate carries two interchangeable surfaces, both backed by SendGrid's
//! v3 [`/mail/send`](https://www.twilio.com/docs/sendgrid/api-reference/mail-send/mail-send)
//! REST endpoint:
//!
//! * The **rich provider** ([`SendGridEmailProvider`]) — the Rust port of
//!   pyfly `pyfly.notifications.providers.sendgrid.SendGridEmailProvider`. It
//!   POSTs a [`EmailMessage`] to SendGrid's v3 `/mail/send` endpoint over
//!   [`reqwest`](https://docs.rs/reqwest), building `personalizations`,
//!   `dynamic_template_data`, and base64 attachments, and parsing the
//!   `X-Message-Id` response header. It implements [`EmailProvider`] and the
//!   [`notifications::Channel`] port.
//! * The **Go-parity envelope adapter** ([`Channel`] / [`Config`] /
//!   [`SendGridChannel`]) — keeps the Go module's [`Config`] wiring surface,
//!   but [`Channel::send`] now performs a **real** `/mail/send` call by mapping
//!   the channel-agnostic [`Notification`] envelope to an [`EmailMessage`] and
//!   delegating to [`SendGridEmailProvider`].
//!
//! Both surfaces talk to the live SendGrid API; the crate no longer ships any
//! not-implemented sentinel. The behavior tests
//! (`tests/mock_send.rs`) point the adapter at an in-process axum mock that
//! asserts the exact outbound request (method, path, auth header, JSON body).
//! A real round trip requires a live `SG.…` API key, so the test suite uses the
//! mock; see the crate README for the live-credential note.
//!
//! # Rich provider example
//!
//! ```
//! use firefly_notifications_sendgrid::{EmailMessage, EmailProvider, SendGridEmailProvider};
//!
//! let provider = SendGridEmailProvider::new("SG.test_key");
//! assert_eq!(provider.name(), "sendgrid");
//! // Sending requires a live (or mocked) SendGrid endpoint; see the crate
//! // tests for an in-process axum mock that asserts the exact JSON payload.
//! let _ = EmailMessage::default();
//! ```
//!
//! # Envelope adapter example (Go-parity [`Config`] wiring)
//!
//! ```no_run
//! use firefly_notifications::{Channel as _, Kind, Notification};
//! use firefly_notifications_sendgrid::{Channel, Config};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let channel = Channel::new(Config {
//!     api_key: "SG.xxxxx".into(),
//!     from_address: "noreply@example.com".into(),
//!     ..Config::default()
//! });
//! assert_eq!(channel.kind(), Kind::EMAIL);
//! assert_eq!(channel.name(), "notificationssendgrid");
//!
//! // Performs a real SendGrid v3 /mail/send call (requires a live API key).
//! channel
//!     .send(Notification {
//!         channel: Kind::EMAIL,
//!         to: "alice@example.com".into(),
//!         subject: "Welcome".into(),
//!         body: "Welcome to Firefly!".into(),
//!         ..Notification::default()
//!     })
//!     .await
//!     .unwrap();
//! # });
//! ```

mod email;

use async_trait::async_trait;
use base64::Engine as _;
use firefly_notifications as notifications;
use firefly_notifications::{DeliveryResult, Kind, Notification, NotificationError};
use serde_json::{json, Value};

pub use email::{Attachment, EmailMessage, EmailProvider, EmailStatus, NotificationResult};

/// The stable provider name used as the `provider` field of results.
pub const PROVIDER_NAME: &str = "sendgrid";

/// The default SendGrid API base (`https://api.sendgrid.com/v3`).
pub const DEFAULT_API_BASE: &str = "https://api.sendgrid.com/v3";

/// Typed configuration carrying the API-key wiring needed by the adapter.
///
/// Field-for-field port of the Go `Config` struct. The SendGrid adapter uses
/// `api_key` and `from_address`; the remaining fields (`from_number`,
/// `account_sid`, `project_id`, `server_key`) mirror the shared vendor-config
/// shape so the configuration surface is uniform across the notification
/// adapter family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// SendGrid API key (`SG.…`).
    pub api_key: String,
    /// Verified sender e-mail address.
    pub from_address: String,
    /// Sender phone number (used by sibling adapters; unused here).
    pub from_number: String,
    /// Vendor account SID (used by sibling adapters; unused here).
    pub account_sid: String,
    /// Vendor project identifier (used by sibling adapters; unused here).
    pub project_id: String,
    /// Vendor server key (used by sibling adapters; unused here).
    pub server_key: String,
}

/// The [`notifications::Channel`] adapter for SendGrid that wires the Go-parity
/// [`Config`] surface to the real [`SendGridEmailProvider`].
///
/// [`Channel::send`] maps the channel-agnostic [`Notification`] envelope to a
/// rich [`EmailMessage`] (using `from_address` as the sender) and POSTs it to
/// SendGrid's v3 `/mail/send` endpoint. A failed delivery surfaces as
/// [`NotificationError::Delivery`] carrying the provider error text.
#[derive(Debug, Clone)]
pub struct Channel {
    cfg: Config,
    provider: SendGridEmailProvider,
}

/// Alias for [`Channel`], useful where importing the bare name would shadow
/// the [`notifications::Channel`] trait.
pub type SendGridChannel = Channel;

impl Channel {
    /// Returns a [`Channel`] bound to a [`SendGridEmailProvider`] built from the
    /// config's `api_key`, targeting the production SendGrid API base.
    pub fn new(cfg: Config) -> Self {
        let provider = SendGridEmailProvider::new(cfg.api_key.clone());
        Self { cfg, provider }
    }

    /// Returns a [`Channel`] pointed at a custom `api_base` (used by tests to
    /// target an in-process mock).
    pub fn with_api_base(cfg: Config, api_base: impl Into<String>) -> Self {
        let provider = SendGridEmailProvider::with_api_base(cfg.api_key.clone(), api_base);
        Self { cfg, provider }
    }

    /// Returns the configuration the channel was constructed with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl notifications::Channel for Channel {
    /// Implements [`notifications::Channel::kind`]; returns [`Kind::EMAIL`].
    fn kind(&self) -> Kind {
        Kind::EMAIL
    }

    /// Implements [`notifications::Channel::send`] by mapping the envelope to an
    /// [`EmailMessage`] (with `from_address` as the sender) and performing a
    /// real SendGrid v3 `/mail/send` call via [`SendGridEmailProvider`].
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::Delivery`] carrying the provider error text
    /// when SendGrid rejects the message or the transport fails.
    async fn send(&self, n: Notification) -> DeliveryResult {
        let mut message = notification_to_email(&n);
        if message.sender.is_empty() {
            message.sender = self.cfg.from_address.clone();
        }
        let result = EmailProvider::send(&self.provider, message).await;
        match result.status {
            EmailStatus::Failed => Err(NotificationError::Delivery(
                result
                    .error
                    .unwrap_or_else(|| "sendgrid delivery failed".into()),
            )),
            _ => Ok(()),
        }
    }

    /// Implements [`notifications::Channel::name`]; returns
    /// `"notificationssendgrid"`.
    fn name(&self) -> String {
        "notificationssendgrid".to_string()
    }
}

// ---------------------------------------------------------------------------
// Rich provider — SendGrid v3 /mail/send.
// ---------------------------------------------------------------------------

/// The SendGrid e-mail provider (pyfly `SendGridEmailProvider`).
///
/// Sends a rich [`EmailMessage`] to SendGrid's v3 `/mail/send` endpoint over
/// HTTPS using a shared [`reqwest::Client`].
#[derive(Debug, Clone)]
pub struct SendGridEmailProvider {
    api_key: String,
    api_base: String,
    http: reqwest::Client,
}

impl SendGridEmailProvider {
    /// Builds a provider for `api_key`, targeting the production SendGrid API
    /// base (`https://api.sendgrid.com/v3`).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_api_base(api_key, DEFAULT_API_BASE)
    }

    /// Builds a provider pointing at a custom `api_base` (used by tests to
    /// target an in-process mock). A trailing slash is trimmed so the joined
    /// `/mail/send` path is well-formed.
    pub fn with_api_base(api_key: impl Into<String>, api_base: impl Into<String>) -> Self {
        SendGridEmailProvider {
            api_key: api_key.into(),
            api_base: api_base.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Builds a provider from flat config keys.
    ///
    /// Recognised keys: `api_key` (required) and `api_base`
    /// (optional; defaults to [`DEFAULT_API_BASE`]).
    pub fn from_config<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let api_key = get("api_key").unwrap_or_default();
        match get("api_base").filter(|v| !v.is_empty()) {
            Some(base) => SendGridEmailProvider::with_api_base(api_key, base),
            None => SendGridEmailProvider::new(api_key),
        }
    }

    /// Builds the SendGrid v3 `/mail/send` JSON payload for `message`.
    ///
    /// Mirrors the pyfly adapter exactly: `personalizations[0]` carries
    /// `to`/`cc`/`bcc`/`subject` (empty cc/bcc are dropped so SendGrid does not
    /// reject null entries); `from` carries the sender; `content` lists the
    /// plain-text part then the HTML part; `template_id` enables provider-native
    /// templates with `dynamic_template_data`; attachments are base64-encoded.
    fn build_payload(message: &EmailMessage) -> Value {
        let mut personalization = json!({
            "to": message.to.iter().map(|e| json!({ "email": e })).collect::<Vec<_>>(),
            "subject": message.subject,
        });
        if !message.cc.is_empty() {
            personalization["cc"] =
                Value::Array(message.cc.iter().map(|e| json!({ "email": e })).collect());
        }
        if !message.bcc.is_empty() {
            personalization["bcc"] =
                Value::Array(message.bcc.iter().map(|e| json!({ "email": e })).collect());
        }

        let mut content = Vec::new();
        if let Some(text) = &message.body_text {
            if !text.is_empty() {
                content.push(json!({ "type": "text/plain", "value": text }));
            }
        }
        if let Some(html) = &message.body_html {
            if !html.is_empty() {
                content.push(json!({ "type": "text/html", "value": html }));
            }
        }

        let mut payload = json!({
            "from": { "email": message.sender },
            "content": content,
        });

        if let Some(template_id) = &message.template_id {
            payload["template_id"] = json!(template_id);
            personalization["dynamic_template_data"] =
                serde_json::to_value(&message.template_data).unwrap_or(Value::Null);
        }

        payload["personalizations"] = json!([personalization]);

        if !message.attachments.is_empty() {
            payload["attachments"] = Value::Array(
                message
                    .attachments
                    .iter()
                    .map(|a| {
                        json!({
                            "filename": a.filename,
                            "type": a.content_type,
                            "content": base64::engine::general_purpose::STANDARD.encode(&a.data),
                        })
                    })
                    .collect(),
            );
        }

        payload
    }
}

#[async_trait]
impl EmailProvider for SendGridEmailProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// Posts `message` to `{api_base}/mail/send`.
    ///
    /// A 2xx response yields [`EmailStatus::Sent`] with the `X-Message-Id`
    /// header as `provider_id`. Any other status — or a transport error —
    /// yields [`EmailStatus::Failed`] carrying `http {status}: {body}` (or the
    /// transport error text). The provider never returns an `Err`, matching
    /// pyfly's `_send_safely` contract.
    async fn send(&self, message: EmailMessage) -> NotificationResult {
        let payload = Self::build_payload(&message);
        let resp = self
            .http
            .post(format!("{}/mail/send", self.api_base))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => return NotificationResult::failed(&message.id, PROVIDER_NAME, e.to_string()),
        };

        let status = resp.status();
        if status.is_success() {
            let provider_id = resp
                .headers()
                .get("X-Message-Id")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            NotificationResult::sent(&message.id, PROVIDER_NAME, provider_id)
        } else {
            let body = resp.text().await.unwrap_or_default();
            NotificationResult::failed(
                &message.id,
                PROVIDER_NAME,
                format!("http {}: {}", status.as_u16(), body),
            )
        }
    }
}

/// Maps a Go-parity [`Notification`] envelope to a rich [`EmailMessage`].
fn notification_to_email(n: &Notification) -> EmailMessage {
    EmailMessage {
        id: if n.id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            n.id.clone()
        },
        to: if n.to.is_empty() {
            Vec::new()
        } else {
            vec![n.to.clone()]
        },
        subject: n.subject.clone(),
        body_text: Some(n.body.clone()),
        template_id: if n.template.is_empty() {
            None
        } else {
            Some(n.template.clone())
        },
        template_data: n.variables.clone().into_iter().collect(),
        ..EmailMessage::default()
    }
}

#[async_trait]
impl notifications::Channel for SendGridEmailProvider {
    /// Implements [`notifications::Channel::kind`]; returns [`Kind::EMAIL`].
    fn kind(&self) -> Kind {
        Kind::EMAIL
    }

    /// Implements [`notifications::Channel::send`] by mapping the thin
    /// envelope to an [`EmailMessage`] and delegating to
    /// [`EmailProvider::send`].
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::Delivery`] carrying the provider error
    /// text when the rich send reports [`EmailStatus::Failed`].
    async fn send(&self, n: Notification) -> DeliveryResult {
        let result = EmailProvider::send(self, notification_to_email(&n)).await;
        match result.status {
            EmailStatus::Failed => Err(NotificationError::Delivery(
                result
                    .error
                    .unwrap_or_else(|| "sendgrid delivery failed".into()),
            )),
            _ => Ok(()),
        }
    }

    /// Implements [`notifications::Channel::name`]; returns `"notificationssendgrid"`.
    fn name(&self) -> String {
        "notificationssendgrid".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use firefly_notifications::Channel as _;

    use super::*;

    // ---------------------------------------------------------------------
    // Port of Go TestImplementsPort: compile-time port satisfaction.
    // ---------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let channel: Arc<dyn notifications::Channel> = Arc::new(Channel::new(Config::default()));
        let _ = &channel;
    }

    // ---------------------------------------------------------------------
    // Rust-specific: channel identity, config plumbing, trait-object
    // usability, Send + Sync bounds. The real HTTP round trip is exercised
    // in tests/mock_send.rs against an in-process axum mock.
    // ---------------------------------------------------------------------

    #[test]
    fn name_matches_real_adapter() {
        assert_eq!(
            Channel::new(Config::default()).name(),
            "notificationssendgrid"
        );
    }

    #[test]
    fn kind_is_email() {
        assert_eq!(Channel::new(Config::default()).kind(), Kind::EMAIL);
        assert_eq!(Channel::new(Config::default()).kind().as_str(), "email");
    }

    #[test]
    fn config_round_trips_through_channel() {
        let cfg = Config {
            api_key: "SG.xxxxx".into(),
            from_address: "noreply@example.com".into(),
            from_number: "+34911".into(),
            account_sid: "AC123".into(),
            project_id: "proj-1".into(),
            server_key: "srv-key".into(),
        };
        let channel = Channel::new(cfg.clone());
        assert_eq!(channel.config(), &cfg);
    }

    #[test]
    fn config_default_is_all_empty() {
        let cfg = Config::default();
        assert!(cfg.api_key.is_empty());
        assert!(cfg.from_address.is_empty());
        assert!(cfg.from_number.is_empty());
        assert!(cfg.account_sid.is_empty());
        assert!(cfg.project_id.is_empty());
        assert!(cfg.server_key.is_empty());
    }

    #[tokio::test]
    async fn channel_send_transport_error_maps_to_delivery_error() {
        // Point the channel at a closed port — a connection-refused transport
        // error must surface as a typed Delivery error, never a panic.
        let channel = Channel::with_api_base(
            Config {
                api_key: "SG.key".into(),
                from_address: "from@x.io".into(),
                ..Config::default()
            },
            "http://127.0.0.1:1",
        );
        let err = channel
            .send(Notification {
                channel: Kind::EMAIL,
                to: "alice@example.com".into(),
                subject: "Welcome".into(),
                body: "hi".into(),
                ..Notification::default()
            })
            .await
            .expect_err("transport error must surface");
        assert!(matches!(err, NotificationError::Delivery(_)), "{err:?}");
    }

    #[test]
    fn channel_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Channel>();
        assert_send_sync::<Config>();
        assert_send_sync::<Arc<dyn notifications::Channel>>();
    }

    // -----------------------------------------------------------------
    // Rich provider unit tests (payload shape + identity; the HTTP round
    // trip lives in tests/mock_send.rs against an in-process axum mock).
    // -----------------------------------------------------------------

    #[test]
    fn provider_identity_and_api_base_trimming() {
        let p = SendGridEmailProvider::new("SG.k");
        assert_eq!(EmailProvider::name(&p), "sendgrid");
        assert_eq!(p.api_base, "https://api.sendgrid.com/v3");

        let p2 = SendGridEmailProvider::with_api_base("SG.k", "http://localhost:8080/");
        assert_eq!(p2.api_base, "http://localhost:8080");
    }

    #[test]
    fn from_config_reads_keys() {
        let map = std::collections::BTreeMap::from([
            ("api_key", "SG.cfg"),
            ("api_base", "http://mock:9/v3"),
        ]);
        let p = SendGridEmailProvider::from_config(|k| map.get(k).map(|v| v.to_string()));
        assert_eq!(p.api_key, "SG.cfg");
        assert_eq!(p.api_base, "http://mock:9/v3");
    }

    #[test]
    fn build_payload_drops_empty_cc_bcc_and_orders_content() {
        let msg = EmailMessage {
            to: vec!["dest@example.com".into()],
            sender: "from@example.com".into(),
            subject: "s".into(),
            body_text: Some("plain".into()),
            body_html: Some("<p>h</p>".into()),
            ..EmailMessage::default()
        };
        let payload = SendGridEmailProvider::build_payload(&msg);
        let p0 = &payload["personalizations"][0];
        assert!(p0.get("cc").is_none());
        assert!(p0.get("bcc").is_none());
        // content order: text/plain first, then text/html
        let content = payload["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text/plain");
        assert_eq!(content[1]["type"], "text/html");
        // no template_id key when not templated
        assert!(payload.get("template_id").is_none());
    }

    #[test]
    fn rich_channel_name_is_distinct_from_stub() {
        let p = SendGridEmailProvider::new("SG.k");
        assert_eq!(notifications::Channel::name(&p), "notificationssendgrid");
        assert_eq!(notifications::Channel::kind(&p), Kind::EMAIL);
    }

    #[test]
    fn rich_provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SendGridEmailProvider>();
        assert_send_sync::<EmailMessage>();
        assert_send_sync::<Arc<dyn EmailProvider>>();
    }
}
