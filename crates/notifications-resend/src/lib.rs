//! firefly-notifications-resend — the placeholder Resend e-mail
//! [`notifications::Channel`].
//!
//! Direct port of the Go `notificationsresend` module (itself a port of the
//! Java `firefly-notifications` Resend adapter and the .NET
//! `FireflyFramework.Notifications.*` project). The integration surface
//! (Resend HTTP API) is in scope for a later milestone — this crate ships the
//! contract-only stub today, exactly as the Go module does.
//!
//! * [`Config`] — typed wiring for the production adapter (API key, sender
//!   address, …).
//! * [`Channel`] — the placeholder port implementation; `send` returns the
//!   [`ERR_NOT_IMPLEMENTED`] sentinel.
//! * [`ERR_NOT_IMPLEMENTED`] / [`not_implemented`] — the wire-stable sentinel,
//!   bytes-equal to the Go module's `ErrNotImplemented`.
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

use async_trait::async_trait;
use firefly_notifications as notifications;
use firefly_notifications::{DeliveryResult, Kind, Notification, NotificationError};

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
}
