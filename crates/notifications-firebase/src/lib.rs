//! firefly-notifications-firebase — the Firebase Cloud Messaging (push)
//! adapter.
//!
//! This crate ships two layers:
//!
//! * The **Go-parity stub** — [`Channel`], the
//!   [`firefly_notifications::Channel`] adapter that routes [`Kind::PUSH`] and
//!   returns the [`ERR_NOT_IMPLEMENTED`] sentinel from [`Channel::send`]. Kept
//!   for backward compatibility with the Go wire contract; consuming code that
//!   wired the stub still compiles and behaves identically.
//! * The **pyfly-parity real provider** — [`FirebasePushProvider`], a working
//!   FCM HTTP v1 integration that implements [`PushProvider`]. It posts once
//!   per device token to `…/v1/projects/{id}/messages:send` with a bearer
//!   token from an injected [`AccessTokenProvider`], and folds per-token
//!   results into a single [`NotificationResult`] with partial-success
//!   semantics.
//!
//! The stub's `ERR_NOT_IMPLEMENTED` sentinel remains byte-for-byte equal to
//! the Go port's `ErrNotImplemented`:
//!
//! ```text
//! firefly/notificationsfirebase: not yet implemented
//! ```
//!
//! # Why ship a stub?
//!
//! * The framework's tier diagram stays correct (no missing module).
//! * The port boundary stays locked — when the real implementation lands,
//!   no consuming code needs to change.
//! * The wire contract is exercised end-to-end before the integration
//!   ships, via the smoke tests that assert the sentinel return.
//!
//! # Quick start
//!
//! ```
//! use firefly_notifications::{Channel as _, Kind, Notification};
//! use firefly_notifications_firebase::{is_not_implemented, Channel, Config};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let channel = Channel::new(Config {
//!     project_id: "firefly-prod".into(),
//!     server_key: "fcm-server-key".into(),
//!     ..Config::default()
//! });
//! assert_eq!(channel.kind(), Kind::PUSH);
//! assert_eq!(channel.name(), "notificationsfirebase-stub");
//!
//! let err = channel.send(Notification::default()).await.unwrap_err();
//! assert!(is_not_implemented(&err));
//! assert_eq!(
//!     err.to_string(),
//!     "firefly/notificationsfirebase: not yet implemented",
//! );
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
pub const VERSION: &str = "26.6.1";

/// The sentinel message returned by [`Channel::send`] until the SaaS HTTP
/// integration is wired.
///
/// Byte-for-byte equal to the Go port's
/// `ErrNotImplemented = errors.New("firefly/notificationsfirebase: not yet implemented")`.
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/notificationsfirebase: not yet implemented";

/// Builds the [`ERR_NOT_IMPLEMENTED`] sentinel as a
/// [`NotificationError::Delivery`] — the value the stubbed [`Channel::send`]
/// returns.
pub fn not_implemented() -> NotificationError {
    NotificationError::Delivery(ERR_NOT_IMPLEMENTED.to_string())
}

/// Returns `true` when `err` is the [`ERR_NOT_IMPLEMENTED`] sentinel — the
/// analog of Go's `errors.Is(err, ErrNotImplemented)`.
pub fn is_not_implemented(err: &NotificationError) -> bool {
    matches!(err, NotificationError::Delivery(msg) if msg == ERR_NOT_IMPLEMENTED)
}

/// Config carries the API-key wiring needed by the production adapter.
///
/// Field-for-field port of the Go `Config` struct. The Firebase adapter uses
/// `project_id` and `server_key`; the remaining fields (`api_key`,
/// `from_address`, `from_number`, `account_sid`) mirror the shared
/// vendor-stub shape so the configuration surface is uniform across the
/// notification adapter family. The stub stores the configuration untouched
/// so consuming code can wire it today and swap in the real adapter without
/// changes.
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
    /// Firebase project identifier.
    pub project_id: String,
    /// Firebase Cloud Messaging server key.
    pub server_key: String,
}

/// Channel is the placeholder [`firefly_notifications::Channel`] adapter for
/// Firebase Cloud Messaging (push).
///
/// [`Channel::send`] returns the [`ERR_NOT_IMPLEMENTED`] sentinel until the
/// production FCM integration is wired.
#[derive(Debug, Clone, Default)]
pub struct Channel {
    cfg: Config,
}

impl Channel {
    /// Returns a placeholder Channel (the analog of Go's `New(cfg)`).
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Returns the configuration the channel was built with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl firefly_notifications::Channel for Channel {
    /// The transport kind: always [`Kind::PUSH`], matching the Go stub.
    fn kind(&self) -> Kind {
        Kind::PUSH
    }

    /// Stubbed: always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
    async fn send(&self, _n: Notification) -> DeliveryResult {
        Err(not_implemented())
    }

    /// Human-readable channel name, matching the Go stub.
    fn name(&self) -> String {
        "notificationsfirebase-stub".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use firefly_notifications::{Channel as ChannelPort, Dispatcher};

    use super::*;

    // -----------------------------------------------------------------------
    // Go: TestImplementsPort — `var _ notifications.Channel = New(Config{})`.
    // The Rust analog: the adapter coerces to the object-safe port behind
    // Box/Arc, which fails to compile if the trait is not implemented.
    // -----------------------------------------------------------------------

    #[test]
    fn implements_port() {
        let boxed: Box<dyn ChannelPort> = Box::new(Channel::new(Config::default()));
        assert_eq!(boxed.name(), "notificationsfirebase-stub");

        let arc: Arc<dyn ChannelPort> = Arc::new(Channel::new(Config::default()));
        assert_eq!(arc.name(), "notificationsfirebase-stub");
    }

    // -----------------------------------------------------------------------
    // Go: TestStubReturnsSentinel — Send returns ErrNotImplemented, Name is
    // non-empty, and Kind is set.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
        let c = Channel::new(Config::default());

        let err = c.send(Notification::default()).await.unwrap_err();
        assert!(is_not_implemented(&err), "Send: {err}");

        assert!(!c.name().is_empty(), "Name should be non-empty");
        assert!(!c.kind().as_str().is_empty(), "Kind should be set");
    }

    // -----------------------------------------------------------------------
    // Rust-specific: sentinel parity, error taxonomy, config plumbing,
    // dispatcher wiring, and auto-trait bounds.
    // -----------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go_byte_for_byte() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/notificationsfirebase: not yet implemented"
        );
        assert_eq!(
            not_implemented().to_string(),
            "firefly/notificationsfirebase: not yet implemented"
        );
    }

    #[test]
    fn sentinel_is_delivery_error_not_no_channel() {
        let err = not_implemented();
        assert!(matches!(err, NotificationError::Delivery(_)));
        assert!(is_not_implemented(&err));

        // Other errors are not mistaken for the sentinel.
        assert!(!is_not_implemented(&NotificationError::NoChannel));
        assert!(!is_not_implemented(&NotificationError::Delivery(
            "other failure".into()
        )));
    }

    #[test]
    fn kind_is_push() {
        let c = Channel::new(Config::default());
        assert_eq!(c.kind(), Kind::PUSH);
        assert_eq!(c.kind().as_str(), "push");
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
        let c = Channel::new(cfg.clone());
        assert_eq!(c.config(), &cfg);

        // Default channel carries the zero config, like Go's New(Config{}).
        assert_eq!(Channel::new(Config::default()).config(), &Config::default());
        assert_eq!(Channel::default().config(), &Config::default());
    }

    // The dispatcher routes push notifications into the stub, which surfaces
    // the sentinel verbatim — the end-to-end wiring consuming code uses today.
    #[tokio::test]
    async fn dispatcher_surfaces_sentinel() {
        let d = Dispatcher::new();
        d.register(Arc::new(Channel::new(Config::default())));

        let err = d
            .dispatch(Notification {
                channel: Kind::PUSH,
                to: "device-token".into(),
                body: "ping".into(),
                ..Notification::default()
            })
            .await
            .unwrap_err();

        assert!(is_not_implemented(&err));
        assert_eq!(
            err.to_string(),
            "firefly/notificationsfirebase: not yet implemented"
        );
    }

    #[test]
    fn channel_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Channel>();
        assert_send_sync::<Config>();
    }
}
