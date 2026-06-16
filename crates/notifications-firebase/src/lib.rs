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

//! firefly-notifications-firebase — the Firebase Cloud Messaging (push)
//! adapter.
//!
//! This crate ships two interchangeable layers, both backed by the FCM
//! [HTTP v1 `messages:send`](https://firebase.google.com/docs/cloud-messaging/send-message)
//! endpoint:
//!
//! * The **Go-parity envelope adapter** — [`Channel`], the
//!   [`firefly_notifications::Channel`] adapter that routes [`Kind::PUSH`]. It
//!   keeps the Go module's [`Config`] wiring surface (`project_id`), and
//!   [`Channel::send`] now performs a **real** `messages:send` POST by mapping
//!   the channel-agnostic [`Notification`] envelope to a [`PushMessage`] and
//!   delegating to [`FirebasePushProvider`].
//! * The **pyfly-parity real provider** — [`FirebasePushProvider`], a working
//!   FCM HTTP v1 integration that implements [`PushProvider`]. It POSTs once
//!   per device token to `…/v1/projects/{id}/messages:send` with a bearer
//!   token from an injected [`AccessTokenProvider`], and folds per-token
//!   results into a single [`NotificationResult`] with partial-success
//!   semantics. It also exposes [`FirebasePushProvider::send_multicast`] (the
//!   multi-token fan-out) and [`FirebasePushProvider::send_to_topic`] (FCM
//!   topic messaging).
//!
//! The crate no longer ships any not-implemented sentinel; every operation
//! calls the real FCM API.
//!
//! # Access-token source
//!
//! FCM v1 authenticates with a short-lived OAuth2 bearer token minted from a
//! Google **service-account** key — *not* the legacy `server_key`. This crate
//! deliberately does **not** implement the service-account JWT → OAuth2
//! exchange (that belongs to a Google-auth library or the GCP metadata server).
//! Instead, both the rich provider and the [`Channel`] adapter take an injected
//! [`AccessTokenProvider`] that yields the current bearer token on each send.
//! Wire it to whatever mints/refreshes tokens in your deployment. Because the
//! Go-parity [`Config`] has no token field, build the [`Channel`] with
//! [`Channel::with_token_provider`] (or [`Channel::with_access_token`] for a
//! fixed token).
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_notifications::{Channel as _, Kind, Notification};
//! use firefly_notifications_firebase::{Channel, Config};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let channel = Channel::with_access_token(
//!     Config {
//!         project_id: "firefly-prod".into(),
//!         ..Config::default()
//!     },
//!     "ya29.short-lived-oauth-token",
//! );
//! assert_eq!(channel.kind(), Kind::PUSH);
//! assert_eq!(channel.name(), "notificationsfirebase");
//!
//! // Performs a real FCM v1 messages:send POST (requires a valid token).
//! channel
//!     .send(Notification {
//!         channel: Kind::PUSH,
//!         to: "device-registration-token".into(),
//!         subject: "Ping".into(),
//!         body: "You have a new message".into(),
//!         ..Notification::default()
//!     })
//!     .await
//!     .unwrap();
//! # }
//! ```

use async_trait::async_trait;
use firefly_notifications::{DeliveryResult, Kind, Notification, NotificationError};

mod provider;

pub use provider::{
    AccessTokenProvider, DeliveryStatus, FirebaseError, FirebasePushProvider, NotificationResult,
    PushMessage, PushProvider, DEFAULT_BASE_URL,
};

/// Framework version stamp.
pub const VERSION: &str = "26.6.21";

/// Config carries the wiring needed by the adapter.
///
/// Field-for-field port of the Go `Config` struct. The Firebase adapter uses
/// `project_id`; the remaining fields (`api_key`, `from_address`,
/// `from_number`, `account_sid`, `server_key`) mirror the shared vendor-config
/// shape so the configuration surface is uniform across the notification
/// adapter family. `server_key` is the legacy FCM credential and is **not**
/// used by the HTTP v1 API — supply an OAuth2 bearer token via
/// [`Channel::with_access_token`] / [`Channel::with_token_provider`] instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Vendor API key (shared adapter-family field).
    pub api_key: String,
    /// Sender e-mail address (shared adapter-family field).
    pub from_address: String,
    /// Sender phone number (shared adapter-family field).
    pub from_number: String,
    /// Account SID (shared adapter-family field).
    pub account_sid: String,
    /// Firebase project identifier (used in the `messages:send` URL).
    pub project_id: String,
    /// Legacy Firebase Cloud Messaging server key (unused by the HTTP v1 API).
    pub server_key: String,
}

/// Channel is the [`firefly_notifications::Channel`] adapter for Firebase Cloud
/// Messaging (push) that wires the Go-parity [`Config`] surface to the real
/// [`FirebasePushProvider`].
///
/// [`Channel::send`] maps the [`Notification`] envelope to a single-token
/// [`PushMessage`] and POSTs it to FCM v1 `messages:send`. Construct it with an
/// access-token source via [`Channel::with_access_token`] (fixed token) or
/// [`Channel::with_token_provider`] (refreshing source); see the crate-level
/// docs on the access-token seam.
#[derive(Clone)]
pub struct Channel {
    cfg: Config,
    provider: FirebasePushProvider,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("cfg", &self.cfg)
            .field("provider", &self.provider)
            .finish()
    }
}

impl Channel {
    /// Returns a Channel that mints its FCM bearer token from `access_token` on
    /// every send (the fixed-token shape).
    pub fn with_access_token(cfg: Config, access_token: impl Into<String>) -> Self {
        let provider = FirebasePushProvider::new(cfg.project_id.clone(), access_token);
        Self { cfg, provider }
    }

    /// Returns a Channel whose FCM bearer token comes from `token_provider`,
    /// invoked once per send so it can refresh.
    pub fn with_token_provider(
        cfg: Config,
        token_provider: impl AccessTokenProvider + 'static,
    ) -> Self {
        let provider =
            FirebasePushProvider::with_token_provider(cfg.project_id.clone(), token_provider);
        Self { cfg, provider }
    }

    /// Points the underlying provider at a custom FCM base URL (used by tests to
    /// target an in-process mock).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.provider = self.provider.with_base_url(base_url);
        self
    }

    /// Returns the configuration the channel was built with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Returns the underlying [`FirebasePushProvider`], e.g. to call
    /// [`FirebasePushProvider::send_to_topic`] or
    /// [`FirebasePushProvider::send_multicast`].
    pub fn provider(&self) -> &FirebasePushProvider {
        &self.provider
    }
}

#[async_trait]
impl firefly_notifications::Channel for Channel {
    /// The transport kind: always [`Kind::PUSH`].
    fn kind(&self) -> Kind {
        Kind::PUSH
    }

    /// Implements [`firefly_notifications::Channel::send`] by mapping the
    /// envelope's `to` to a single device token and performing a real FCM v1
    /// `messages:send` POST via [`FirebasePushProvider`].
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::Delivery`] when the access-token provider or
    /// transport fails, or when FCM rejects the message (a non-2xx response
    /// folds into the provider's `FAILED` result, whose error text is surfaced).
    async fn send(&self, n: Notification) -> DeliveryResult {
        let message = PushMessage {
            id: if n.id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                n.id.clone()
            },
            device_tokens: if n.to.is_empty() {
                Vec::new()
            } else {
                vec![n.to.clone()]
            },
            title: n.subject.clone(),
            body: n.body.clone(),
            data: serde_json::Map::new(),
        };
        match self.provider.send(message).await {
            Ok(result) => match result.status {
                DeliveryStatus::Failed => Err(NotificationError::Delivery(
                    result
                        .error
                        .unwrap_or_else(|| "firebase delivery failed".into()),
                )),
                _ => Ok(()),
            },
            Err(e) => Err(NotificationError::Delivery(e.to_string())),
        }
    }

    /// Human-readable channel name.
    fn name(&self) -> String {
        "notificationsfirebase".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use firefly_notifications::{Channel as ChannelPort, Dispatcher};

    use super::*;

    fn test_channel(cfg: Config) -> Channel {
        Channel::with_access_token(cfg, "ya29.test-token")
    }

    // -----------------------------------------------------------------------
    // Go: TestImplementsPort — `var _ notifications.Channel = New(Config{})`.
    // The Rust analog: the adapter coerces to the object-safe port behind
    // Box/Arc, which fails to compile if the trait is not implemented.
    // -----------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let boxed: Box<dyn ChannelPort> = Box::new(test_channel(Config::default()));
        assert_eq!(boxed.name(), "notificationsfirebase");

        let arc: Arc<dyn ChannelPort> = Arc::new(test_channel(Config::default()));
        assert_eq!(arc.name(), "notificationsfirebase");
    }

    // -----------------------------------------------------------------------
    // Rust-specific: config plumbing, dispatcher wiring, and auto-trait
    // bounds. The real HTTP round trip (send, multicast, topic) is exercised
    // in tests/firebase_behavior.rs against an in-process axum mock.
    // -----------------------------------------------------------------------

    #[test]
    fn kind_is_push() {
        let c = test_channel(Config::default());
        assert_eq!(c.kind(), Kind::PUSH);
        assert_eq!(c.kind().as_str(), "push");
        assert_eq!(c.name(), "notificationsfirebase");
    }

    #[test]
    fn config_round_trips_through_constructor() {
        let cfg = Config {
            api_key: "key".into(),
            from_address: "no-reply@example.com".into(),
            from_number: "+34911".into(),
            account_sid: "AC123".into(),
            project_id: "firefly-prod".into(),
            server_key: "fcm-server-key".into(),
        };
        let c = test_channel(cfg.clone());
        assert_eq!(c.config(), &cfg);
        assert_eq!(c.provider().name(), "firebase");
    }

    // The dispatcher routes push notifications into the channel; against a
    // closed port the send surfaces a typed Delivery error.
    #[tokio::test]
    async fn dispatcher_routes_push_and_surfaces_delivery_error() {
        let d = Dispatcher::new();
        let channel = Channel::with_access_token(
            Config {
                project_id: "firefly-prod".into(),
                ..Config::default()
            },
            "ya29.token",
        )
        .with_base_url("http://127.0.0.1:1");
        d.register(Arc::new(channel));

        let err = d
            .dispatch(Notification {
                channel: Kind::PUSH,
                to: "device-token".into(),
                body: "ping".into(),
                ..Notification::default()
            })
            .await
            .expect_err("connection-refused push must fail");

        assert!(matches!(err, NotificationError::Delivery(_)), "{err:?}");
    }

    #[test]
    fn channel_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Channel>();
        assert_send_sync::<Config>();
    }
}
