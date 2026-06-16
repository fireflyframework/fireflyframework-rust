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

//! firefly-notifications — the channel-agnostic notification port.
//!
//! This crate is the Rust counterpart of the Go module
//! `github.com/fireflyframework/fireflyframework-go/notifications`:
//!
//! * [`Notification`] — the message envelope (channel, recipient, body,
//!   optional template, optional variables).
//! * [`Channel`] — the transport interface (`kind`, `send`, `name`).
//! * [`Dispatcher`] — fans messages out to channels keyed on [`Kind`].
//! * [`MemoryChannel`] — a default in-process channel that records every
//!   message sent (useful for tests).
//!
//! Concrete provider adapters (`firefly-notifications-sendgrid`,
//! `firefly-notifications-resend`, `firefly-notifications-twilio`,
//! `firefly-notifications-firebase`) live in dedicated crates.
//!
//! # pyfly parity layer
//!
//! Alongside the Go-parity envelope above, this crate ships the richer
//! `pyfly.notifications` surface (kept fully separate — the
//! [`Notification`]/[`Dispatcher`]/[`Channel`]/[`MemoryChannel`] types are
//! unchanged):
//!
//! * **Models** ([`models`]): [`DeliveryStatus`], [`Attachment`],
//!   [`EmailMessage`], [`SmsMessage`], [`PushMessage`], [`NotificationResult`].
//! * **Ports** ([`ports`]): [`EmailProvider`]/[`SmsProvider`]/[`PushProvider`]
//!   and [`EmailService`]/[`SmsService`]/[`PushService`].
//! * **Services** ([`services`]): [`DefaultEmailService`],
//!   [`DefaultSmsService`], [`DefaultPushService`] — opt-out pruning, template
//!   precedence, metrics, and provider-error-to-`FAILED` conversion.
//! * **Preferences** ([`preferences`]): [`PreferenceService`] +
//!   [`InMemoryPreferenceService`].
//! * **Templates** ([`template`]): [`TemplateEngine`], [`NoOpTemplateEngine`],
//!   and (feature `minijinja`) [`MiniJinjaTemplateEngine`].
//! * **Metrics** ([`metrics`]): the [`NotificationMetrics`] hook +
//!   [`InMemoryNotificationMetrics`].
//! * **Dummy providers** ([`dummy`]): [`DummyEmailProvider`],
//!   [`DummySmsProvider`], [`DummyPushProvider`].
//! * **Config selection** ([`config`]): `from_config` provider/engine/store
//!   selection helpers.
//!
//! # Quick start
//!
//! ```
//! use std::sync::Arc;
//!
//! use firefly_notifications::{Dispatcher, Kind, MemoryChannel, Notification};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let dispatcher = Dispatcher::new();
//! let email = Arc::new(MemoryChannel::new(Kind::EMAIL));
//! dispatcher.register(email.clone());
//!
//! dispatcher
//!     .dispatch(Notification {
//!         channel: Kind::EMAIL,
//!         to: "alice@example.com".into(),
//!         subject: "Welcome".into(),
//!         body: "Welcome to Firefly!".into(),
//!         ..Notification::default()
//!     })
//!     .await
//!     .unwrap();
//!
//! assert_eq!(email.messages().len(), 1);
//! # }
//! ```

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Framework version stamp.
pub const VERSION: &str = "26.6.18";

/// Errors produced by the notification port.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotificationError {
    /// No channel is registered for the message's [`Kind`].
    ///
    /// Mirrors the Go sentinel `ErrNoChannel`, including its exact message.
    #[error("firefly/notifications: no channel registered")]
    NoChannel,
    /// A channel accepted the message but delivery failed.
    ///
    /// The payload carries the transport's own error message verbatim
    /// (provider adapters prefix it with their module path, exactly as
    /// the Go adapters do with their module-scoped sentinels).
    #[error("{0}")]
    Delivery(String),
}

/// The result of a single delivery attempt — `Ok(())` on success.
pub type DeliveryResult = Result<(), NotificationError>;

/// Kind enumerates the canonical channels.
///
/// Like the Go `type Kind string`, this is an open string newtype: the
/// canonical values are [`Kind::EMAIL`], [`Kind::SMS`], and [`Kind::PUSH`],
/// but custom transports may introduce their own kinds via [`Kind::new`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Kind(Cow<'static, str>);

impl Kind {
    /// The canonical e-mail channel (`"email"`).
    pub const EMAIL: Kind = Kind(Cow::Borrowed("email"));
    /// The canonical SMS channel (`"sms"`).
    pub const SMS: Kind = Kind(Cow::Borrowed("sms"));
    /// The canonical push channel (`"push"`).
    pub const PUSH: Kind = Kind(Cow::Borrowed("push"));

    /// Returns a custom kind for transports outside the canonical set.
    pub fn new(kind: impl Into<Cow<'static, str>>) -> Self {
        Kind(kind.into())
    }

    /// Returns the kind's wire value (`"email"`, `"sms"`, …).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Kind {
    /// The empty kind — the Go zero value of `Kind`.
    fn default() -> Self {
        Kind(Cow::Borrowed(""))
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&'static str> for Kind {
    fn from(kind: &'static str) -> Self {
        Kind(Cow::Borrowed(kind))
    }
}

impl From<String> for Kind {
    fn from(kind: String) -> Self {
        Kind(Cow::Owned(kind))
    }
}

/// Notification is the channel-agnostic message envelope.
///
/// The JSON shape matches the Go struct field-for-field: empty `subject`,
/// `template`, and `variables` are omitted, and `created_at` marshals as
/// RFC 3339 under the `createdAt` key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Caller-assigned message identifier.
    #[serde(default)]
    pub id: String,
    /// The transport kind this message should be routed to.
    #[serde(default)]
    pub channel: Kind,
    /// Recipient address (e-mail address, phone number, device token…).
    #[serde(default)]
    pub to: String,
    /// Optional subject line; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    /// Message body.
    #[serde(default)]
    pub body: String,
    /// Optional template name; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub template: String,
    /// Optional template variables; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub variables: HashMap<String, serde_json::Value>,
    /// Creation timestamp, serialized as `createdAt`.
    #[serde(rename = "createdAt", default = "go_zero_time")]
    pub created_at: DateTime<Utc>,
}

impl Default for Notification {
    /// The zero envelope — every field empty and `created_at` set to the
    /// Go zero time (`0001-01-01T00:00:00Z`) so that the JSON of a default
    /// `Notification` is byte-identical across ports.
    fn default() -> Self {
        Notification {
            id: String::new(),
            channel: Kind::default(),
            to: String::new(),
            subject: String::new(),
            body: String::new(),
            template: String::new(),
            variables: HashMap::new(),
            created_at: go_zero_time(),
        }
    }
}

/// The Go `time.Time` zero value, used so default envelopes marshal
/// identically to the Go port (`"0001-01-01T00:00:00Z"`).
fn go_zero_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0)
        .single()
        .expect("the Go zero time is a valid UTC instant")
}

/// Channel is a single transport (SendGrid, Twilio, Firebase…).
#[async_trait]
pub trait Channel: Send + Sync {
    /// The [`Kind`] this channel handles.
    fn kind(&self) -> Kind;
    /// Delivers `n` over the transport.
    async fn send(&self, n: Notification) -> DeliveryResult;
    /// Human-readable channel name.
    fn name(&self) -> String;
}

/// Dispatcher fans messages out to channels keyed on [`Kind`].
#[derive(Default)]
pub struct Dispatcher {
    channels: RwLock<HashMap<Kind, Arc<dyn Channel>>>,
}

impl Dispatcher {
    /// Returns an empty `Dispatcher`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs `channel` for `channel.kind()`. Calling `register` twice
    /// for the same kind overwrites the previous channel.
    pub fn register(&self, channel: Arc<dyn Channel>) {
        let kind = channel.kind();
        self.channels
            .write()
            .expect("dispatcher lock poisoned")
            .insert(kind, channel);
    }

    /// Sends `n` via the registered channel.
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::NoChannel`] when no channel is
    /// registered for `n.channel`, or the channel's own error when
    /// delivery fails.
    pub async fn dispatch(&self, n: Notification) -> DeliveryResult {
        let channel = {
            let channels = self.channels.read().expect("dispatcher lock poisoned");
            channels.get(&n.channel).cloned()
        };
        match channel {
            Some(c) => c.send(n).await,
            None => Err(NotificationError::NoChannel),
        }
    }
}

/// MemoryChannel is an in-process channel that records every message
/// sent — used in tests.
pub struct MemoryChannel {
    kind: Kind,
    messages: Mutex<Vec<Notification>>,
}

impl MemoryChannel {
    /// Returns a `MemoryChannel` for `kind`.
    pub fn new(kind: Kind) -> Self {
        MemoryChannel {
            kind,
            messages: Mutex::new(Vec::new()),
        }
    }

    /// Returns a snapshot of every message sent so far, in order.
    pub fn messages(&self) -> Vec<Notification> {
        self.messages
            .lock()
            .expect("memory channel lock poisoned")
            .clone()
    }
}

#[async_trait]
impl Channel for MemoryChannel {
    fn kind(&self) -> Kind {
        self.kind.clone()
    }

    async fn send(&self, n: Notification) -> DeliveryResult {
        self.messages
            .lock()
            .expect("memory channel lock poisoned")
            .push(n);
        Ok(())
    }

    fn name(&self) -> String {
        format!("memory-{}", self.kind)
    }
}

// ---------------------------------------------------------------------------
// pyfly parity layer (rich channel-specific notifications).
// ---------------------------------------------------------------------------

pub mod config;
pub mod dummy;
pub mod metrics;
pub mod models;
pub mod ports;
pub mod preferences;
pub mod services;
pub mod template;

pub use config::{
    EmailProviderSelection, PreferenceStoreSelection, PushProviderSelection, SmsProviderSelection,
    TemplateEngineSelection,
};
pub use dummy::{DummyEmailProvider, DummyPushProvider, DummySmsProvider};
pub use metrics::{InMemoryNotificationMetrics, NotificationMetrics};
pub use models::{
    Attachment, DeliveryStatus, EmailMessage, NotificationResult, PushMessage, SmsMessage,
};
pub use ports::{EmailProvider, EmailService, PushProvider, PushService, SmsProvider, SmsService};
pub use preferences::{InMemoryPreferenceService, PreferenceService};
pub use services::{DefaultEmailService, DefaultPushService, DefaultSmsService};
#[cfg(feature = "minijinja")]
pub use template::MiniJinjaTemplateEngine;
pub use template::{NoOpTemplateEngine, TemplateEngine, TemplateError};

#[cfg(test)]
mod tests {
    use super::*;

    // Port of Go TestDispatcherRoutes.
    #[tokio::test]
    async fn dispatcher_routes() {
        let d = Dispatcher::new();
        let email = Arc::new(MemoryChannel::new(Kind::EMAIL));
        let sms = Arc::new(MemoryChannel::new(Kind::SMS));
        d.register(email.clone());
        d.register(sms.clone());

        d.dispatch(Notification {
            channel: Kind::EMAIL,
            to: "a@b.co".into(),
            body: "hi".into(),
            ..Notification::default()
        })
        .await
        .expect("email dispatch");
        d.dispatch(Notification {
            channel: Kind::SMS,
            to: "+34911".into(),
            body: "hi".into(),
            ..Notification::default()
        })
        .await
        .expect("sms dispatch");

        assert_eq!(email.messages().len(), 1);
        assert_eq!(sms.messages().len(), 1);

        let err = d
            .dispatch(Notification {
                channel: Kind::PUSH,
                ..Notification::default()
            })
            .await
            .expect_err("push has no channel");
        assert_eq!(err, NotificationError::NoChannel);
    }

    #[tokio::test]
    async fn register_overwrites_previous_channel_for_same_kind() {
        let d = Dispatcher::new();
        let first = Arc::new(MemoryChannel::new(Kind::EMAIL));
        let second = Arc::new(MemoryChannel::new(Kind::EMAIL));
        d.register(first.clone());
        d.register(second.clone());

        d.dispatch(Notification {
            channel: Kind::EMAIL,
            to: "a@b.co".into(),
            body: "hi".into(),
            ..Notification::default()
        })
        .await
        .expect("dispatch");

        assert!(first.messages().is_empty());
        assert_eq!(second.messages().len(), 1);
    }

    #[tokio::test]
    async fn memory_channel_records_full_envelope() {
        let d = Dispatcher::new();
        let email = Arc::new(MemoryChannel::new(Kind::EMAIL));
        d.register(email.clone());

        let sent = Notification {
            id: "n-1".into(),
            channel: Kind::EMAIL,
            to: "alice@example.com".into(),
            subject: "Welcome".into(),
            body: "Welcome to Firefly!".into(),
            template: "welcome-v1".into(),
            variables: HashMap::from([("name".to_string(), serde_json::json!("Alice"))]),
            created_at: Utc.with_ymd_and_hms(2026, 6, 12, 10, 30, 0).unwrap(),
        };
        d.dispatch(sent.clone()).await.expect("dispatch");

        assert_eq!(email.messages(), vec![sent]);
    }

    #[tokio::test]
    async fn concurrent_dispatch_records_every_message() {
        let d = Arc::new(Dispatcher::new());
        let email = Arc::new(MemoryChannel::new(Kind::EMAIL));
        d.register(email.clone());

        let mut handles = Vec::new();
        for i in 0..16 {
            let d = d.clone();
            handles.push(tokio::spawn(async move {
                d.dispatch(Notification {
                    id: format!("n-{i}"),
                    channel: Kind::EMAIL,
                    to: "a@b.co".into(),
                    body: "hi".into(),
                    ..Notification::default()
                })
                .await
            }));
        }
        for h in handles {
            h.await.expect("join").expect("dispatch");
        }

        assert_eq!(email.messages().len(), 16);
    }

    #[test]
    fn memory_channel_name_and_kind() {
        let m = MemoryChannel::new(Kind::EMAIL);
        assert_eq!(m.kind(), Kind::EMAIL);
        assert_eq!(m.name(), "memory-email");
        assert_eq!(MemoryChannel::new(Kind::SMS).name(), "memory-sms");
    }

    #[test]
    fn kind_constants_match_go_wire_values() {
        assert_eq!(Kind::EMAIL.as_str(), "email");
        assert_eq!(Kind::SMS.as_str(), "sms");
        assert_eq!(Kind::PUSH.as_str(), "push");
        assert_eq!(Kind::default().as_str(), "");
        assert_eq!(Kind::new("webhook"), Kind::from("webhook"));
        assert_eq!(Kind::from("email".to_string()), Kind::EMAIL);
        assert_eq!(Kind::PUSH.to_string(), "push");
    }

    #[test]
    fn error_messages_match_go_sentinels() {
        assert_eq!(
            NotificationError::NoChannel.to_string(),
            "firefly/notifications: no channel registered"
        );
        assert_eq!(
            NotificationError::Delivery(
                "firefly/notificationssendgrid: not yet implemented".into()
            )
            .to_string(),
            "firefly/notificationssendgrid: not yet implemented"
        );
    }

    // Wire-format parity: json.Marshal(Notification{}) in Go.
    #[test]
    fn zero_notification_json_matches_go() {
        let json = serde_json::to_string(&Notification::default()).expect("marshal");
        assert_eq!(
            json,
            r#"{"id":"","channel":"","to":"","body":"","createdAt":"0001-01-01T00:00:00Z"}"#
        );
    }

    // Wire-format parity: full envelope, optional fields present.
    #[test]
    fn full_notification_json_matches_go() {
        let n = Notification {
            id: "n-1".into(),
            channel: Kind::EMAIL,
            to: "alice@example.com".into(),
            subject: "Welcome".into(),
            body: "Welcome to Firefly!".into(),
            template: "welcome-v1".into(),
            variables: HashMap::from([("name".to_string(), serde_json::json!("Alice"))]),
            created_at: Utc.with_ymd_and_hms(2026, 6, 12, 10, 30, 0).unwrap(),
        };
        let json = serde_json::to_string(&n).expect("marshal");
        assert_eq!(
            json,
            r#"{"id":"n-1","channel":"email","to":"alice@example.com","subject":"Welcome","body":"Welcome to Firefly!","template":"welcome-v1","variables":{"name":"Alice"},"createdAt":"2026-06-12T10:30:00Z"}"#
        );
    }

    #[test]
    fn notification_serde_round_trip() {
        let n = Notification {
            id: "n-2".into(),
            channel: Kind::PUSH,
            to: "device-token".into(),
            body: "ping".into(),
            variables: HashMap::from([("count".to_string(), serde_json::json!(3))]),
            created_at: Utc.with_ymd_and_hms(2026, 6, 12, 9, 0, 0).unwrap(),
            ..Notification::default()
        };
        let json = serde_json::to_string(&n).expect("marshal");
        let back: Notification = serde_json::from_str(&json).expect("unmarshal");
        assert_eq!(back, n);
    }

    // Optional fields absent in incoming JSON deserialize to their zero values.
    #[test]
    fn notification_deserializes_go_minimal_json() {
        let back: Notification = serde_json::from_str(
            r#"{"id":"","channel":"sms","to":"+34911","body":"hi","createdAt":"0001-01-01T00:00:00Z"}"#,
        )
        .expect("unmarshal");
        assert_eq!(back.channel, Kind::SMS);
        assert!(back.subject.is_empty());
        assert!(back.template.is_empty());
        assert!(back.variables.is_empty());
        assert_eq!(back.created_at, go_zero_time());
    }

    #[test]
    fn port_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Dispatcher>();
        assert_send_sync::<MemoryChannel>();
        assert_send_sync::<Notification>();
        assert_send_sync::<Kind>();
        assert_send_sync::<NotificationError>();
        assert_send_sync::<Arc<dyn Channel>>();
    }
}
