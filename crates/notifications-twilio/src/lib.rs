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

//! firefly-notifications-twilio — the Twilio SMS adapter.
//!
//! This crate ships two interchangeable layers, both backed by Twilio's
//! [Programmable Messaging REST API](https://www.twilio.com/docs/sms/api/message-resource):
//!
//! * The **Go-parity envelope adapter** — [`Channel`], the
//!   [`notifications::Channel`] adapter that routes [`Kind::SMS`]. It keeps the
//!   Go module's [`Config`] wiring surface, and [`notifications::Channel::send`]
//!   now performs a **real** `Messages.json` POST by mapping the channel-agnostic
//!   [`Notification`] envelope to an [`SmsMessage`] and delegating to
//!   [`TwilioSmsProvider`].
//! * The **pyfly-parity real provider** — [`TwilioSmsProvider`], a working
//!   HTTP integration that implements [`SmsProvider`]. It POSTs to Twilio's
//!   `Messages.json` endpoint with HTTP basic auth and a form-encoded body,
//!   parses the `sid` into a [`NotificationResult`], and folds non-2xx
//!   responses into a [`DeliveryStatus::Failed`] result. It also exposes
//!   [`TwilioSmsProvider::fetch_status`], a `GET` against the Message resource
//!   that returns the current [`MessageStatus`] (`delivered`/`failed`/…).
//!
//! Direct port of the Go module `fireflyframework-go/notificationstwilio` and
//! of `pyfly.notifications.providers.twilio`, themselves ports of the Java
//! `firefly-notifications-twilio` module and the .NET
//! `FireflyFramework.Notifications.*` project. The crate no longer ships any
//! not-implemented sentinel; every operation calls the real Twilio API.
//!
//! * [`Config`] — typed wiring for the adapter (account SID, sender number, …).
//! * [`Channel`] — the [`notifications::Channel`] port; routes [`Kind::SMS`] and
//!   performs a real send.
//!
//! A real round trip requires live Twilio credentials, so the test suite points
//! the adapter at an in-process axum mock that asserts the exact request bytes;
//! see the crate README for the live-credential note.
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_notifications::{Channel as _, Kind, Notification};
//! use firefly_notifications_twilio::{Channel, Config};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let channel = Channel::new(Config {
//!     account_sid: "AC0000".into(),
//!     api_key: "your-auth-token".into(),
//!     from_number: "+15550100".into(),
//!     ..Config::default()
//! });
//!
//! assert_eq!(channel.kind(), Kind::SMS);
//! assert_eq!(channel.name(), "notificationstwilio");
//!
//! // Performs a real Twilio Messages.json POST (requires live credentials).
//! channel
//!     .send(Notification {
//!         channel: Kind::SMS,
//!         to: "+15559876543".into(),
//!         body: "hello from Firefly".into(),
//!         ..Notification::default()
//!     })
//!     .await
//!     .unwrap();
//! # }
//! ```

use async_trait::async_trait;
use firefly_notifications as notifications;
use firefly_notifications::{DeliveryResult, Kind, Notification, NotificationError};

mod provider;

pub use provider::{
    DeliveryStatus, MessageStatus, NotificationResult, SmsMessage, SmsProvider, TwilioError,
    TwilioSmsProvider, DEFAULT_BASE_URL,
};

/// Config carries the API-key wiring needed by the adapter.
///
/// The fields cover every wiring variable the adapter needs; the
/// non-Twilio-flavoured fields exist because the Java module shares one
/// configuration surface across the notification provider adapters. For Twilio,
/// `account_sid` and `api_key` (the auth token) authenticate the request and
/// `from_number` is the default sender.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Twilio auth token (HTTP basic-auth password; the Go field is `api_key`).
    pub api_key: String,
    /// Sender e-mail address (shared surface; unused by the SMS adapter).
    pub from_address: String,
    /// Sender phone number in E.164 form (e.g. `+15550100`).
    pub from_number: String,
    /// Twilio account SID (HTTP basic-auth username and URL segment).
    pub account_sid: String,
    /// Project identifier (shared surface; Firebase flavour).
    pub project_id: String,
    /// Server key (shared surface; Firebase flavour).
    pub server_key: String,
}

/// Channel is the [`notifications::Channel`] adapter for Twilio (SMS) that wires
/// the Go-parity [`Config`] surface to the real [`TwilioSmsProvider`].
///
/// [`notifications::Channel::kind`] routes [`Kind::SMS`];
/// [`notifications::Channel::send`] maps the [`Notification`] envelope to an
/// [`SmsMessage`] and POSTs it to Twilio's `Messages.json` endpoint with HTTP
/// basic auth.
#[derive(Debug, Clone)]
pub struct Channel {
    cfg: Config,
    provider: TwilioSmsProvider,
}

impl Channel {
    /// Returns a Channel bound to a [`TwilioSmsProvider`] built from the config's
    /// `account_sid`, `api_key` (auth token), and `from_number`, targeting the
    /// production Twilio API host.
    pub fn new(cfg: Config) -> Self {
        let provider = Self::build_provider(&cfg, None);
        Self { cfg, provider }
    }

    /// Returns a Channel pointed at a custom `base_url` (used by tests to target
    /// an in-process mock).
    pub fn with_base_url(cfg: Config, base_url: impl Into<String>) -> Self {
        let provider = Self::build_provider(&cfg, Some(base_url.into()));
        Self { cfg, provider }
    }

    fn build_provider(cfg: &Config, base_url: Option<String>) -> TwilioSmsProvider {
        let mut provider = TwilioSmsProvider::new(cfg.account_sid.clone(), cfg.api_key.clone());
        if !cfg.from_number.is_empty() {
            provider = provider.with_from_number(cfg.from_number.clone());
        }
        if let Some(url) = base_url {
            provider = provider.with_base_url(url);
        }
        provider
    }

    /// The configuration this channel was constructed with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Returns the underlying [`TwilioSmsProvider`], e.g. to call
    /// [`TwilioSmsProvider::fetch_status`].
    pub fn provider(&self) -> &TwilioSmsProvider {
        &self.provider
    }
}

#[async_trait]
impl notifications::Channel for Channel {
    /// Implements [`notifications::Channel::kind`]; always [`Kind::SMS`].
    fn kind(&self) -> Kind {
        Kind::SMS
    }

    /// Implements [`notifications::Channel::send`] by mapping the envelope to an
    /// [`SmsMessage`] and performing a real Twilio `Messages.json` POST via
    /// [`TwilioSmsProvider`].
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::Delivery`] when no sender can be resolved,
    /// when the transport fails, or when Twilio rejects the message (a non-2xx
    /// response maps to the provider's `FAILED` result, whose error text is
    /// surfaced verbatim).
    async fn send(&self, n: Notification) -> DeliveryResult {
        let message = SmsMessage {
            id: if n.id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                n.id.clone()
            },
            to: n.to.clone(),
            body: n.body.clone(),
            sender: None,
        };
        match self.provider.send(message).await {
            Ok(result) => match result.status {
                DeliveryStatus::Failed => Err(NotificationError::Delivery(
                    result
                        .error
                        .unwrap_or_else(|| "twilio delivery failed".into()),
                )),
                _ => Ok(()),
            },
            Err(e) => Err(NotificationError::Delivery(e.to_string())),
        }
    }

    /// Implements [`notifications::Channel::name`].
    fn name(&self) -> String {
        "notificationstwilio".to_string()
    }
}

/// Framework version stamp.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// Compile-time port assertion — the analog of Go's
// `var _ notifications.Channel = (*Channel)(nil)`.
const _: () = {
    const fn assert_port<T: notifications::Channel>() {}
    assert_port::<Channel>();
};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use firefly_notifications::{Channel as ChannelPort, Dispatcher};

    use super::*;

    // -------------------------------------------------------------------
    // Ported from adapter_test.go
    // -------------------------------------------------------------------

    /// Go: `TestImplementsPort` — compile-time port satisfaction
    /// (`var _ notifications.Channel = New(Config{})`).
    #[test]
    fn implements_port() {
        fn assert_port<T: ChannelPort>() {}
        assert_port::<Channel>();

        // And the trait stays object-safe behind Box/Arc, like Go's interface.
        let _boxed: Box<dyn ChannelPort> = Box::new(Channel::new(Config::default()));
        let _shared: Arc<dyn ChannelPort> = Arc::new(Channel::new(Config::default()));
    }

    // -------------------------------------------------------------------
    // Rust-specific additions. The real HTTP round trip (send + status
    // fetch) is exercised in tests/twilio_behavior.rs against an in-process
    // axum mock.
    // -------------------------------------------------------------------

    #[test]
    fn kind_routes_sms_and_name_matches_real_adapter() {
        let c = Channel::new(Config::default());
        assert_eq!(c.kind(), Kind::SMS);
        assert_eq!(c.kind().as_str(), "sms");
        assert_eq!(c.name(), "notificationstwilio");
    }

    #[test]
    fn config_is_retained_for_the_adapter() {
        let cfg = Config {
            api_key: "sk-test".into(),
            from_address: "noreply@example.com".into(),
            from_number: "+15550100".into(),
            account_sid: "AC0000".into(),
            project_id: "proj-1".into(),
            server_key: "srv-key".into(),
        };
        let c = Channel::new(cfg.clone());
        assert_eq!(c.config(), &cfg);
        assert_eq!(c.provider().name(), "twilio");

        // The zero config mirrors Go's `Config{}`.
        assert_eq!(
            Config::default(),
            Config {
                api_key: String::new(),
                from_address: String::new(),
                from_number: String::new(),
                account_sid: String::new(),
                project_id: String::new(),
                server_key: String::new(),
            }
        );
    }

    /// Registering the channel with the dispatcher routes SMS messages into it.
    /// Against a closed port the send surfaces a typed Delivery error; other
    /// kinds stay unrouted.
    #[tokio::test]
    async fn dispatcher_routes_sms_into_channel_and_surfaces_delivery_error() {
        let d = Dispatcher::new();
        d.register(Arc::new(Channel::with_base_url(
            Config {
                account_sid: "AC0000".into(),
                api_key: "tok".into(),
                from_number: "+15550100".into(),
                ..Config::default()
            },
            "http://127.0.0.1:1",
        )));

        let err = d
            .dispatch(Notification {
                channel: Kind::SMS,
                to: "+34911".into(),
                body: "hi".into(),
                ..Notification::default()
            })
            .await
            .expect_err("connection-refused send must fail");
        assert!(matches!(err, NotificationError::Delivery(_)), "{err:?}");

        // Other kinds stay unrouted: the channel registers only Kind::SMS.
        let err = d
            .dispatch(Notification {
                channel: Kind::EMAIL,
                ..Notification::default()
            })
            .await
            .expect_err("email has no channel");
        assert_eq!(err, NotificationError::NoChannel);
    }

    /// A missing sender (no from_number, no per-message sender) surfaces as a
    /// typed Delivery error before any HTTP call.
    #[tokio::test]
    async fn send_without_sender_surfaces_delivery_error() {
        let c = Channel::new(Config {
            account_sid: "AC0000".into(),
            api_key: "tok".into(),
            // no from_number
            ..Config::default()
        });
        let err = c
            .send(Notification {
                channel: Kind::SMS,
                to: "+34911".into(),
                body: "hi".into(),
                ..Notification::default()
            })
            .await
            .expect_err("missing sender must error");
        match err {
            NotificationError::Delivery(msg) => assert!(msg.contains("needs a sender"), "{msg}"),
            other => panic!("want Delivery, got {other:?}"),
        }
    }

    #[test]
    fn channel_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Channel>();
        assert_send_sync::<Config>();
        assert_send_sync::<Arc<dyn ChannelPort>>();
    }

    #[test]
    fn version_stamp() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
