//! Rich e-mail domain model — the Rust port of pyfly
//! `pyfly.notifications.models` (the email-relevant subset) and
//! `pyfly.notifications.ports.EmailProvider`.
//!
//! The core [`firefly_notifications`] crate ships the channel-agnostic
//! Go-parity envelope ([`firefly_notifications::Notification`]); the richer
//! channel-specific message model lives beside each provider so the provider
//! crates stay independent of one another. The types here are byte-for-byte
//! the same shape across `firefly-notifications-sendgrid`,
//! `firefly-notifications-resend`, and `firefly-notifications-smtp` so the
//! provider surface is uniform.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Delivery status of an e-mail (pyfly `EmailStatus`).
///
/// The wire values are the upper-case names from the pyfly `StrEnum`
/// (`"QUEUED"`, `"SENT"`, …) so serialized results match across ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmailStatus {
    /// Accepted by the service, not yet handed to a provider.
    #[serde(rename = "QUEUED")]
    Queued,
    /// Handed to the provider successfully.
    #[serde(rename = "SENT")]
    Sent,
    /// Confirmed delivered by the provider.
    #[serde(rename = "DELIVERED")]
    Delivered,
    /// Rejected by the recipient's mail server.
    #[serde(rename = "BOUNCED")]
    Bounced,
    /// Delivery failed (transport or provider error).
    #[serde(rename = "FAILED")]
    Failed,
    /// Suppressed by an opt-out / preference check.
    #[serde(rename = "SUPPRESSED")]
    Suppressed,
}

impl EmailStatus {
    /// The pyfly wire string for this status (`"SENT"`, `"FAILED"`, …).
    pub fn as_str(&self) -> &'static str {
        match self {
            EmailStatus::Queued => "QUEUED",
            EmailStatus::Sent => "SENT",
            EmailStatus::Delivered => "DELIVERED",
            EmailStatus::Bounced => "BOUNCED",
            EmailStatus::Failed => "FAILED",
            EmailStatus::Suppressed => "SUPPRESSED",
        }
    }
}

/// A file attached to an [`EmailMessage`] (pyfly `Attachment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// File name presented to the recipient.
    pub filename: String,
    /// MIME content type (`text/plain`, `application/pdf`, …).
    pub content_type: String,
    /// Raw bytes of the attachment (providers base64-encode as needed).
    pub data: Vec<u8>,
}

impl Attachment {
    /// Builds an attachment from a filename, content type, and raw bytes.
    pub fn new(
        filename: impl Into<String>,
        content_type: impl Into<String>,
        data: impl Into<Vec<u8>>,
    ) -> Self {
        Attachment {
            filename: filename.into(),
            content_type: content_type.into(),
            data: data.into(),
        }
    }
}

/// A rich e-mail message (pyfly `EmailMessage`).
///
/// Mirrors the pyfly dataclass field-for-field: multiple `to`/`cc`/`bcc`
/// recipients, separate plain-text and HTML bodies, attachments, custom
/// headers, and optional provider-native template routing
/// (`template_id` + `template_data`).
#[derive(Debug, Clone, PartialEq)]
pub struct EmailMessage {
    /// Caller- or framework-assigned identifier (defaults to a fresh UUIDv4).
    pub id: String,
    /// Primary recipients.
    pub to: Vec<String>,
    /// Carbon-copy recipients.
    pub cc: Vec<String>,
    /// Blind-carbon-copy recipients (never leaked to other recipients).
    pub bcc: Vec<String>,
    /// Sender address.
    pub sender: String,
    /// Subject line.
    pub subject: String,
    /// Optional plain-text body.
    pub body_text: Option<String>,
    /// Optional HTML body.
    pub body_html: Option<String>,
    /// Attachments.
    pub attachments: Vec<Attachment>,
    /// Custom headers (insertion order is not significant; reserved headers
    /// such as `From`/`To`/`Cc`/`Bcc`/`Subject` are ignored by providers).
    pub headers: BTreeMap<String, String>,
    /// Optional provider-native template id (e.g. a SendGrid Dynamic Template).
    pub template_id: Option<String>,
    /// Substitution data for `template_id`.
    pub template_data: BTreeMap<String, serde_json::Value>,
}

impl Default for EmailMessage {
    /// An empty message with a freshly generated UUIDv4 `id`, matching the
    /// pyfly `field(default_factory=lambda: str(uuid.uuid4()))` default.
    fn default() -> Self {
        EmailMessage {
            id: uuid::Uuid::new_v4().to_string(),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            sender: String::new(),
            subject: String::new(),
            body_text: None,
            body_html: None,
            attachments: Vec::new(),
            headers: BTreeMap::new(),
            template_id: None,
            template_data: BTreeMap::new(),
        }
    }
}

impl EmailMessage {
    /// A new empty message (with a fresh `id`).
    pub fn new() -> Self {
        Self::default()
    }
}

/// The outcome of a single provider send (pyfly `NotificationResult`).
///
/// pyfly services and providers never raise to the caller: a transport or
/// provider error is folded into a [`EmailStatus::Failed`] result carrying the
/// error text. This crate preserves that contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationResult {
    /// The originating message's `id`.
    pub id: String,
    /// The provider that produced this result (`"sendgrid"`, `"smtp"`, …).
    pub provider: String,
    /// Final delivery status.
    pub status: EmailStatus,
    /// Provider-assigned message identifier, when the provider returns one
    /// (SendGrid `X-Message-Id`, Resend `id`, …). `None` on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Human-readable error message; `None` unless `status` is
    /// [`EmailStatus::Failed`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl NotificationResult {
    /// A successful result for `id`/`provider` with an optional provider id.
    pub fn sent(
        id: impl Into<String>,
        provider: impl Into<String>,
        provider_id: Option<String>,
    ) -> Self {
        NotificationResult {
            id: id.into(),
            provider: provider.into(),
            status: EmailStatus::Sent,
            provider_id,
            error: None,
        }
    }

    /// A failed result for `id`/`provider` carrying `error`.
    pub fn failed(
        id: impl Into<String>,
        provider: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        NotificationResult {
            id: id.into(),
            provider: provider.into(),
            status: EmailStatus::Failed,
            provider_id: None,
            error: Some(error.into()),
        }
    }
}

/// The e-mail delivery port (pyfly `EmailProvider` protocol).
///
/// Implementors deliver a single [`EmailMessage`] and report the outcome as a
/// [`NotificationResult`]. Per the pyfly contract, `send` must NOT surface
/// transport errors as `Err`; a failed delivery is reported as
/// `Ok(NotificationResult { status: Failed, .. })`.
#[async_trait]
pub trait EmailProvider: Send + Sync {
    /// Stable provider name used as the `provider` field of results
    /// (`"sendgrid"`, `"resend"`, `"smtp"`).
    fn name(&self) -> &str;

    /// Delivers `message`, reporting the outcome as a [`NotificationResult`].
    async fn send(&self, message: EmailMessage) -> NotificationResult;
}
