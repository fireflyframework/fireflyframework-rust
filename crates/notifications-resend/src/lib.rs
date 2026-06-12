//! firefly-notifications-resend — the Resend e-mail adapter.
//!
//! This crate carries two surfaces:
//!
//! * The **rich provider** ([`ResendEmailProvider`]) — the Rust port of
//!   pyfly `pyfly.notifications.providers.resend.ResendEmailProvider`. It
//!   POSTs a rich [`EmailMessage`] to Resend's `/emails` endpoint over
//!   [`reqwest`](https://docs.rs/reqwest), supporting cc/bcc, separate
//!   text/HTML bodies, base64 attachments, and an optional `default_from`
//!   fallback, and parsing the `id` field of the JSON response. It implements
//!   [`EmailProvider`] and a thin [`notifications::Channel`] mapping.
//! * The **Go-parity stub** ([`Channel`] / [`Config`] /
//!   [`ERR_NOT_IMPLEMENTED`] / [`not_implemented`]) — kept verbatim for
//!   backward compatibility with the Go wire contract. Its [`Channel::send`]
//!   still returns the sentinel.
//!
//! # Rich provider example
//!
//! ```
//! use firefly_notifications_resend::{EmailMessage, EmailProvider, ResendEmailProvider};
//!
//! let provider = ResendEmailProvider::new("re_test_key");
//! assert_eq!(provider.name(), "resend");
//! // Sending requires a live (or mocked) Resend endpoint; see the crate tests
//! // for an in-process axum mock that asserts the exact JSON payload.
//! let _ = EmailMessage::default();
//! ```
//!
//! # Stub example (Go parity)
//!
//! ```
//! use firefly_notifications::{Channel as _, Notification};
//! use firefly_notifications_resend::{Channel, Config, ERR_NOT_IMPLEMENTED};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let channel = Channel::new(Config {
//!     api_key: "re_123".into(),
//!     from_address: "no-reply@example.com".into(),
//!     ..Config::default()
//! });
//! assert_eq!(channel.name(), "notificationsresend-stub");
//!
//! let err = channel.send(Notification::default()).await.unwrap_err();
//! assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
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
pub const PROVIDER_NAME: &str = "resend";

/// The default Resend API base (`https://api.resend.com`).
pub const DEFAULT_API_BASE: &str = "https://api.resend.com";

/// The sentinel message returned by `send` until the SaaS HTTP integration
/// is wired.
///
/// Bytes-equal to the Go module's `ErrNotImplemented`:
///
/// ```go
/// var ErrNotImplemented = errors.New("firefly/notificationsresend: not yet implemented")
/// ```
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/notificationsresend: not yet implemented";

/// Builds the not-implemented sentinel as a
/// [`NotificationError::Delivery`] carrying [`ERR_NOT_IMPLEMENTED`] verbatim —
/// the Rust analog of comparing against the Go `ErrNotImplemented` value with
/// `errors.Is`.
pub fn not_implemented() -> NotificationError {
    NotificationError::Delivery(ERR_NOT_IMPLEMENTED.to_string())
}

/// Typed configuration carrying the API-key wiring needed by the production
/// adapter.
///
/// Field-for-field port of the Go `Config` struct. The Resend adapter uses
/// `api_key` and `from_address`; the remaining fields (`from_number`,
/// `account_sid`, `project_id`, `server_key`) mirror the shared vendor-stub
/// shape so the configuration surface is uniform across the notification
/// adapter family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Resend API key (`re_…`).
    pub api_key: String,
    /// Sender e-mail address.
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

/// The placeholder [`notifications::Channel`] adapter for Resend (e-mail).
///
/// `send` returns the [`ERR_NOT_IMPLEMENTED`] sentinel (wrapped in
/// [`NotificationError::Delivery`]) until the production integration ships.
#[derive(Debug, Clone)]
pub struct Channel {
    cfg: Config,
}

impl Channel {
    /// Returns a placeholder [`Channel`] holding the given wiring.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
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

    /// Implements [`notifications::Channel::send`]; always returns the
    /// sentinel.
    async fn send(&self, _n: Notification) -> DeliveryResult {
        Err(not_implemented())
    }

    /// Implements [`notifications::Channel::name`]; returns
    /// `"notificationsresend-stub"`.
    fn name(&self) -> String {
        "notificationsresend-stub".to_string()
    }
}

// ---------------------------------------------------------------------------
// Rich provider — Resend /emails.
// ---------------------------------------------------------------------------

/// The Resend e-mail provider (pyfly `ResendEmailProvider`).
///
/// POSTs a rich [`EmailMessage`] to Resend's `/emails` endpoint over HTTPS
/// using a shared [`reqwest::Client`].
#[derive(Debug, Clone)]
pub struct ResendEmailProvider {
    api_key: String,
    api_base: String,
    default_from: Option<String>,
    http: reqwest::Client,
}

impl ResendEmailProvider {
    /// Builds a provider for `api_key`, targeting the production Resend API
    /// base (`https://api.resend.com`) with no default sender.
    pub fn new(api_key: impl Into<String>) -> Self {
        ResendEmailProvider {
            api_key: api_key.into(),
            api_base: DEFAULT_API_BASE.to_string(),
            default_from: None,
            http: reqwest::Client::new(),
        }
    }

    /// Sets the fallback sender used when a message's `sender` is empty
    /// (pyfly `default_from`).
    pub fn with_default_from(mut self, default_from: impl Into<String>) -> Self {
        self.default_from = Some(default_from.into());
        self
    }

    /// Points the provider at a custom `api_base` (used by tests to target an
    /// in-process mock). A trailing slash is trimmed.
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into().trim_end_matches('/').to_string();
        self
    }

    /// Builds a provider from flat config keys.
    ///
    /// Recognised keys: `api_key` (required), `api_base`
    /// (optional; defaults to [`DEFAULT_API_BASE`]), and `default_from`
    /// (optional fallback sender).
    pub fn from_config<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut provider = ResendEmailProvider::new(get("api_key").unwrap_or_default());
        if let Some(base) = get("api_base").filter(|v| !v.is_empty()) {
            provider = provider.with_api_base(base);
        }
        if let Some(from) = get("default_from").filter(|v| !v.is_empty()) {
            provider = provider.with_default_from(from);
        }
        provider
    }

    /// Builds the Resend `/emails` JSON payload for `message`.
    ///
    /// Mirrors the pyfly adapter exactly: `from` is `message.sender` or the
    /// configured `default_from`; `to` is the recipient list; `cc`/`bcc` are
    /// added only when non-empty; `text`/`html` are added only when present;
    /// attachments carry `filename` and base64 `content` (Resend does not take
    /// a `type` field, unlike SendGrid).
    fn build_payload(&self, message: &EmailMessage) -> Value {
        let from = if message.sender.is_empty() {
            self.default_from.clone()
        } else {
            Some(message.sender.clone())
        };
        let mut payload = json!({
            "from": from,
            "to": message.to,
            "subject": message.subject,
        });
        if !message.cc.is_empty() {
            payload["cc"] = json!(message.cc);
        }
        if !message.bcc.is_empty() {
            payload["bcc"] = json!(message.bcc);
        }
        if let Some(text) = &message.body_text {
            if !text.is_empty() {
                payload["text"] = json!(text);
            }
        }
        if let Some(html) = &message.body_html {
            if !html.is_empty() {
                payload["html"] = json!(html);
            }
        }
        if !message.attachments.is_empty() {
            payload["attachments"] = Value::Array(
                message
                    .attachments
                    .iter()
                    .map(|a| {
                        json!({
                            "filename": a.filename,
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
impl EmailProvider for ResendEmailProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// Posts `message` to `{api_base}/emails`.
    ///
    /// A 2xx response yields [`EmailStatus::Sent`] with the response JSON's
    /// `id` field as `provider_id`. Any other status — or a transport error —
    /// yields [`EmailStatus::Failed`] carrying `http {status}: {body}` (or the
    /// transport error text). The provider never returns an `Err`.
    async fn send(&self, message: EmailMessage) -> NotificationResult {
        let payload = self.build_payload(&message);
        let resp = self
            .http
            .post(format!("{}/emails", self.api_base))
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
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            let provider_id = body
                .get("id")
                .and_then(|v| v.as_str())
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
impl notifications::Channel for ResendEmailProvider {
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
                    .unwrap_or_else(|| "resend delivery failed".into()),
            )),
            _ => Ok(()),
        }
    }

    /// Implements [`notifications::Channel::name`]; returns `"notificationsresend"`.
    fn name(&self) -> String {
        "notificationsresend".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use firefly_notifications::{Dispatcher, MemoryChannel};

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
    // Port of Go TestStubReturnsSentinel: Send yields the sentinel and the
    // Name/Kind accessors are populated.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
        use notifications::Channel as _;

        let c = Channel::new(Config::default());
        assert_eq!(
            c.send(Notification::default()).await.unwrap_err(),
            not_implemented(),
            "Send: want ErrNotImplemented"
        );
        assert!(!c.name().is_empty(), "Name should be non-empty");
        assert!(!c.kind().as_str().is_empty(), "Kind should be set");
    }

    // ---------------------------------------------------------------------
    // Rust-specific: sentinel wire shape, channel name/kind, config
    // plumbing, dispatcher integration, and Send + Sync bounds.
    // ---------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/notificationsresend: not yet implemented"
        );
        assert_eq!(not_implemented().to_string(), ERR_NOT_IMPLEMENTED);
    }

    #[test]
    fn sentinel_is_delivery_variant() {
        match not_implemented() {
            NotificationError::Delivery(msg) => assert_eq!(msg, ERR_NOT_IMPLEMENTED),
            other => panic!("want Delivery variant, got {other:?}"),
        }
    }

    #[test]
    fn name_matches_go() {
        use notifications::Channel as _;
        assert_eq!(
            Channel::new(Config::default()).name(),
            "notificationsresend-stub"
        );
    }

    #[test]
    fn kind_is_email() {
        use notifications::Channel as _;
        assert_eq!(Channel::new(Config::default()).kind(), Kind::EMAIL);
    }

    #[test]
    fn config_round_trips_through_channel() {
        let cfg = Config {
            api_key: "re_123".into(),
            from_address: "no-reply@example.com".into(),
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

    // The dispatcher routes email traffic to the stub and surfaces the
    // sentinel verbatim, while other kinds remain unaffected.
    #[tokio::test]
    async fn dispatcher_surfaces_sentinel() {
        let d = Dispatcher::new();
        d.register(Arc::new(Channel::new(Config::default())));
        let sms = Arc::new(MemoryChannel::new(Kind::SMS));
        d.register(sms.clone());

        let err = d
            .dispatch(Notification {
                channel: Kind::EMAIL,
                to: "alice@example.com".into(),
                body: "hi".into(),
                ..Notification::default()
            })
            .await
            .expect_err("stub email channel must fail");
        assert_eq!(err, not_implemented());
        assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);

        d.dispatch(Notification {
            channel: Kind::SMS,
            to: "+34911".into(),
            body: "hi".into(),
            ..Notification::default()
        })
        .await
        .expect("sms dispatch");
        assert_eq!(sms.messages().len(), 1);
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let channel: Arc<dyn notifications::Channel> = Arc::new(Channel::new(Config::default()));
        assert_eq!(channel.name(), "notificationsresend-stub");
        assert_eq!(channel.kind(), Kind::EMAIL);
        assert_eq!(
            channel.send(Notification::default()).await.unwrap_err(),
            not_implemented()
        );
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
        let p = ResendEmailProvider::new("re_k");
        assert_eq!(EmailProvider::name(&p), "resend");
        assert_eq!(p.api_base, "https://api.resend.com");

        let p2 = ResendEmailProvider::new("re_k").with_api_base("http://localhost:8080/");
        assert_eq!(p2.api_base, "http://localhost:8080");
    }

    #[test]
    fn from_config_reads_keys() {
        let map = std::collections::BTreeMap::from([
            ("api_key", "re_cfg"),
            ("api_base", "http://mock:9"),
            ("default_from", "noreply@x.io"),
        ]);
        let p = ResendEmailProvider::from_config(|k| map.get(k).map(|v| v.to_string()));
        assert_eq!(p.api_key, "re_cfg");
        assert_eq!(p.api_base, "http://mock:9");
        assert_eq!(p.default_from.as_deref(), Some("noreply@x.io"));
    }

    #[test]
    fn build_payload_omits_empty_fields_and_uses_default_from() {
        let provider = ResendEmailProvider::new("re_k").with_default_from("default@x.io");
        let msg = EmailMessage {
            to: vec!["dest@example.com".into()],
            // sender empty -> default_from is used
            subject: "Hello".into(),
            body_text: Some("plain body".into()),
            ..EmailMessage::default()
        };
        let payload = provider.build_payload(&msg);
        assert_eq!(payload["from"], "default@x.io");
        assert_eq!(payload["to"], json!(["dest@example.com"]));
        assert_eq!(payload["subject"], "Hello");
        assert_eq!(payload["text"], "plain body");
        assert!(payload.get("html").is_none());
        assert!(payload.get("cc").is_none());
        assert!(payload.get("bcc").is_none());
        assert!(payload.get("attachments").is_none());
    }

    #[test]
    fn build_payload_attachment_has_no_type_field() {
        let provider = ResendEmailProvider::new("re_k");
        let msg = EmailMessage {
            to: vec!["a@x.io".into()],
            sender: "s@x.io".into(),
            subject: "s".into(),
            attachments: vec![Attachment::new("f.txt", "text/plain", b"hi".to_vec())],
            ..EmailMessage::default()
        };
        let payload = provider.build_payload(&msg);
        let att = &payload["attachments"][0];
        assert!(att.get("filename").is_some());
        assert!(att.get("content").is_some());
        // Resend's payload omits the SendGrid-style `type` key
        assert!(att.get("type").is_none());
    }

    #[test]
    fn rich_channel_name_is_distinct_from_stub() {
        let p = ResendEmailProvider::new("re_k");
        assert_eq!(notifications::Channel::name(&p), "notificationsresend");
        assert_eq!(notifications::Channel::kind(&p), Kind::EMAIL);
    }

    #[test]
    fn rich_provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ResendEmailProvider>();
        assert_send_sync::<EmailMessage>();
        assert_send_sync::<Arc<dyn EmailProvider>>();
    }
}
