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

//! Rich channel-specific notification message models (pyfly parity).
//!
//! These types are the Rust counterpart of `pyfly.notifications.models`:
//! a [`DeliveryStatus`] lifecycle enum, an [`Attachment`], the three
//! channel-specific messages ([`EmailMessage`], [`SmsMessage`],
//! [`PushMessage`]), and a [`NotificationResult`] describing the outcome of a
//! single send.
//!
//! They live alongside — and are entirely independent of — the Go-parity
//! [`Notification`](crate::Notification) envelope, which keeps its single-`to`,
//! single-`body` wire shape untouched.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle status of a notification send.
///
/// Mirrors pyfly's `EmailStatus` `StrEnum` exactly — the wire values are the
/// upper-case names (`"QUEUED"`, `"SENT"`, …) and a notification can be:
///
/// * [`DeliveryStatus::Queued`] — accepted but not yet handed to a transport;
/// * [`DeliveryStatus::Sent`] — handed to the provider successfully;
/// * [`DeliveryStatus::Delivered`] — confirmed delivered to the recipient;
/// * [`DeliveryStatus::Bounced`] — rejected by the recipient's system;
/// * [`DeliveryStatus::Failed`] — the send attempt errored;
/// * [`DeliveryStatus::Suppressed`] — short-circuited because every recipient
///   has opted out (the provider was never called).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeliveryStatus {
    /// Accepted but not yet handed to a transport.
    #[serde(rename = "QUEUED")]
    Queued,
    /// Handed to the provider successfully.
    #[serde(rename = "SENT")]
    Sent,
    /// Confirmed delivered to the recipient.
    #[serde(rename = "DELIVERED")]
    Delivered,
    /// Rejected by the recipient's system.
    #[serde(rename = "BOUNCED")]
    Bounced,
    /// The send attempt errored.
    #[serde(rename = "FAILED")]
    Failed,
    /// Short-circuited because every recipient has opted out.
    #[serde(rename = "SUPPRESSED")]
    Suppressed,
}

impl DeliveryStatus {
    /// Returns the wire value of the status (`"QUEUED"`, `"SENT"`, …),
    /// identical to pyfly's `StrEnum` value.
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

impl std::fmt::Display for DeliveryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A file attached to an [`EmailMessage`].
///
/// `data` carries the raw (un-encoded) attachment bytes; vendor adapters are
/// responsible for any transport-specific encoding (e.g. base64 for SendGrid).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// File name as it should appear to the recipient.
    pub filename: String,
    /// MIME content type (e.g. `"application/pdf"`).
    pub content_type: String,
    /// Raw attachment bytes.
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

/// Generates a fresh random message id (UUID v4 string), matching pyfly's
/// `field(default_factory=lambda: str(uuid.uuid4()))`.
fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// A rich e-mail message: multiple recipients, cc/bcc, separate text and HTML
/// bodies, attachments, custom headers, and provider-native template routing.
///
/// Equivalent to pyfly's `EmailMessage` dataclass. Construct via
/// [`EmailMessage::default`] / [`EmailMessage::new`] and the field setters, or
/// the builder methods, then fill fields directly — every field is public.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailMessage {
    /// Caller- or auto-assigned message id (UUID v4 by default).
    pub id: String,
    /// Primary recipients.
    pub to: Vec<String>,
    /// Carbon-copy recipients.
    pub cc: Vec<String>,
    /// Blind-carbon-copy recipients (never leaked in transport headers).
    pub bcc: Vec<String>,
    /// Sender address.
    pub sender: String,
    /// Subject line.
    pub subject: String,
    /// Plain-text body, if any.
    pub body_text: Option<String>,
    /// HTML body, if any.
    pub body_html: Option<String>,
    /// File attachments.
    pub attachments: Vec<Attachment>,
    /// Custom (non-reserved) headers.
    pub headers: HashMap<String, String>,
    /// Provider-native template id (e.g. a SendGrid Dynamic Template id).
    pub template_id: Option<String>,
    /// Variables for the provider-native template.
    pub template_data: HashMap<String, serde_json::Value>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

impl Default for EmailMessage {
    fn default() -> Self {
        EmailMessage {
            id: new_id(),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            sender: String::new(),
            subject: String::new(),
            body_text: None,
            body_html: None,
            attachments: Vec::new(),
            headers: HashMap::new(),
            template_id: None,
            template_data: HashMap::new(),
            created_at: Utc::now(),
        }
    }
}

impl EmailMessage {
    /// Returns a fresh, empty email message with a new random id.
    pub fn new() -> Self {
        Self::default()
    }
}

/// An SMS message — a single recipient and a single body.
///
/// Equivalent to pyfly's `SmsMessage` dataclass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmsMessage {
    /// Caller- or auto-assigned message id (UUID v4 by default).
    pub id: String,
    /// Recipient phone number.
    pub to: String,
    /// Message body.
    pub body: String,
    /// Optional explicit sender (overrides the provider default).
    pub sender: Option<String>,
}

impl Default for SmsMessage {
    fn default() -> Self {
        SmsMessage {
            id: new_id(),
            to: String::new(),
            body: String::new(),
            sender: None,
        }
    }
}

impl SmsMessage {
    /// Returns a fresh, empty SMS message with a new random id.
    pub fn new() -> Self {
        Self::default()
    }
}

/// A push notification — fanned out to one or more device tokens.
///
/// Equivalent to pyfly's `PushMessage` dataclass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushMessage {
    /// Caller- or auto-assigned message id (UUID v4 by default).
    pub id: String,
    /// Target device tokens.
    pub device_tokens: Vec<String>,
    /// Notification title.
    pub title: String,
    /// Notification body.
    pub body: String,
    /// Free-form data payload delivered alongside the notification.
    pub data: HashMap<String, serde_json::Value>,
}

impl Default for PushMessage {
    fn default() -> Self {
        PushMessage {
            id: new_id(),
            device_tokens: Vec::new(),
            title: String::new(),
            body: String::new(),
            data: HashMap::new(),
        }
    }
}

impl PushMessage {
    /// Returns a fresh, empty push message with a new random id.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The outcome of a single notification send.
///
/// Equivalent to pyfly's `NotificationResult` dataclass: it carries the
/// originating message `id`, the `provider` name, the resulting
/// [`DeliveryStatus`], an optional provider-side id, and an optional error
/// string (set when [`DeliveryStatus::Failed`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationResult {
    /// The id of the message this result describes.
    pub id: String,
    /// The provider that produced this result.
    pub provider: String,
    /// The delivery status.
    pub status: DeliveryStatus,
    /// The provider-side id (e.g. SendGrid's `X-Message-Id`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// The error message, if the send failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl NotificationResult {
    /// Builds a successful (`SENT`) result with an optional provider id.
    pub fn sent(
        id: impl Into<String>,
        provider: impl Into<String>,
        provider_id: Option<String>,
    ) -> Self {
        NotificationResult {
            id: id.into(),
            provider: provider.into(),
            status: DeliveryStatus::Sent,
            provider_id,
            error: None,
        }
    }

    /// Builds a [`DeliveryStatus::Suppressed`] result (provider never called).
    pub fn suppressed(id: impl Into<String>, provider: impl Into<String>) -> Self {
        NotificationResult {
            id: id.into(),
            provider: provider.into(),
            status: DeliveryStatus::Suppressed,
            provider_id: None,
            error: None,
        }
    }

    /// Builds a [`DeliveryStatus::Failed`] result carrying the error string.
    pub fn failed(
        id: impl Into<String>,
        provider: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        NotificationResult {
            id: id.into(),
            provider: provider.into(),
            status: DeliveryStatus::Failed,
            provider_id: None,
            error: Some(error.into()),
        }
    }
}
