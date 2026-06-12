//! firefly-notifications-sendgrid — the placeholder
//! [`notifications::Channel`] adapter for SendGrid (email).
//!
//! Direct port of the Go `notificationssendgrid` module (itself a port of the
//! Java `firefly-notifications-*` adapter family). The integration surface
//! (SendGrid v3 Mail Send HTTP API) is in scope for a later milestone — this
//! crate ships the contract-only stub today, exactly as the Go module does.
//!
//! * [`Config`] — typed wiring for the production adapter (API key, sender
//!   address, …).
//! * [`Channel`] — the placeholder port implementation; [`Channel::send`]
//!   returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
//! * [`ERR_NOT_IMPLEMENTED`] / [`not_implemented`] — the wire-stable sentinel,
//!   bytes-equal to the Go module's `ErrNotImplemented`.
//! * [`SendGridChannel`] — Rust-side alias for [`Channel`], handy where the
//!   bare name would shadow the `firefly_notifications::Channel` trait.
//!
//! # Why ship a stub?
//!
//! * The framework's tier diagram stays correct (no missing crate).
//! * The port boundary stays locked — when the real implementation lands,
//!   no consuming code needs to change.
//! * The wire contract is exercised end-to-end before the integration ships,
//!   via the smoke tests that assert the sentinel return.
//!
//! # Example
//!
//! ```
//! use firefly_notifications::{Channel as _, Kind, Notification};
//! use firefly_notifications_sendgrid::{not_implemented, Channel, Config};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let channel = Channel::new(Config {
//!     api_key: "SG.xxxxx".into(),
//!     from_address: "noreply@example.com".into(),
//!     ..Config::default()
//! });
//! assert_eq!(channel.kind(), Kind::EMAIL);
//! assert_eq!(channel.name(), "notificationssendgrid-stub");
//!
//! let err = channel.send(Notification::default()).await.unwrap_err();
//! assert_eq!(err, not_implemented());
//! # });
//! ```

use async_trait::async_trait;
use firefly_notifications as notifications;
use firefly_notifications::{DeliveryResult, Kind, Notification, NotificationError};

/// The sentinel message returned by [`Channel::send`] until the SaaS HTTP
/// integration is wired.
///
/// Bytes-equal to the Go module's `ErrNotImplemented`:
///
/// ```go
/// var ErrNotImplemented = errors.New("firefly/notificationssendgrid: not yet implemented")
/// ```
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/notificationssendgrid: not yet implemented";

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
/// Field-for-field port of the Go `Config` struct. The SendGrid adapter uses
/// `api_key` and `from_address`; the remaining fields (`from_number`,
/// `account_sid`, `project_id`, `server_key`) mirror the shared vendor-stub
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

/// The placeholder [`notifications::Channel`] adapter for SendGrid.
///
/// [`Channel::send`] returns the [`ERR_NOT_IMPLEMENTED`] sentinel (wrapped in
/// [`NotificationError::Delivery`]) until the production integration ships.
#[derive(Debug, Clone)]
pub struct Channel {
    cfg: Config,
}

/// Alias for [`Channel`], useful where importing the bare name would shadow
/// the [`notifications::Channel`] trait.
pub type SendGridChannel = Channel;

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
    /// `"notificationssendgrid-stub"`.
    fn name(&self) -> String {
        "notificationssendgrid-stub".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use firefly_notifications::{Channel as _, Dispatcher};

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
    // identity accessors are populated.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn stub_returns_sentinel() {
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
    // Rust-specific: sentinel wire shape, channel identity, config plumbing,
    // dispatcher integration, trait-object usability, Send + Sync bounds.
    // ---------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/notificationssendgrid: not yet implemented"
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
        assert_eq!(
            Channel::new(Config::default()).name(),
            "notificationssendgrid-stub"
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
    async fn dispatcher_routes_email_to_stub_and_surfaces_sentinel() {
        let d = Dispatcher::new();
        d.register(Arc::new(Channel::new(Config::default())));

        let err = d
            .dispatch(Notification {
                channel: Kind::EMAIL,
                to: "alice@example.com".into(),
                subject: "Welcome".into(),
                body: "Welcome to Firefly!".into(),
                ..Notification::default()
            })
            .await
            .expect_err("stub delivery must fail");
        assert_eq!(err, not_implemented());
        assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let channel: Arc<dyn notifications::Channel> = Arc::new(Channel::new(Config::default()));
        assert_eq!(channel.name(), "notificationssendgrid-stub");
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
}
