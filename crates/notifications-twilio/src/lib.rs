//! firefly-notifications-twilio — the [`notifications::Channel`] adapter for
//! Twilio (SMS).
//!
//! Direct port of the Go module `fireflyframework-go/notificationstwilio`,
//! itself a port of the Java `firefly-notifications-twilio` module and the
//! .NET `FireflyFramework.Notifications.*` project. The SaaS HTTP integration
//! is in scope for a later milestone — this crate ships the contract-only
//! stub: the types are declared, the port is satisfied, and sentinel-error
//! smoke tests guard the wire shape, but [`notifications::Channel::send`]
//! always returns the [`ERR_NOT_IMPLEMENTED`] sentinel.
//!
//! The sentinel message is bytes-equal to the Go port's `ErrNotImplemented`
//! (`firefly/notificationstwilio: not yet implemented`), carried through
//! [`NotificationError::Delivery`] so consumers can match on the rendered
//! message exactly as Go callers match with `errors.Is`.
//!
//! * [`Config`] — typed wiring for the production adapter (API key, sender
//!   address/number, account SID, …).
//! * [`Channel`] — the placeholder port implementation; routes [`Kind::SMS`],
//!   answers [`notifications::Channel::name`], and returns the sentinel from
//!   [`notifications::Channel::send`].
//! * [`ERR_NOT_IMPLEMENTED`] / [`err_not_implemented`] — the wire-stable
//!   sentinel, bytes-equal to the Go module's `ErrNotImplemented`.
//!
//! # Why ship a stub?
//!
//! * The framework's tier diagram stays correct (no missing crate).
//! * The port boundary stays locked — when the real implementation lands,
//!   no consuming code needs to change.
//! * The wire contract is exercised end-to-end before the integration ships,
//!   via the smoke tests that assert the sentinel return.
//!
//! # Quick start
//!
//! ```
//! use firefly_notifications::{Channel as _, Kind, Notification};
//! use firefly_notifications_twilio::{Channel, Config, ERR_NOT_IMPLEMENTED};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let channel = Channel::new(Config {
//!     account_sid: "AC0000".into(),
//!     api_key: "sk-test".into(),
//!     from_number: "+15550100".into(),
//!     ..Config::default()
//! });
//!
//! assert_eq!(channel.kind(), Kind::SMS);
//! assert_eq!(channel.name(), "notificationstwilio-stub");
//!
//! // Send returns the sentinel until the SaaS HTTP integration is wired.
//! let err = channel.send(Notification::default()).await.unwrap_err();
//! assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
//! # }
//! ```

use async_trait::async_trait;
use firefly_notifications as notifications;
use firefly_notifications::{DeliveryResult, Kind, Notification, NotificationError};

/// The sentinel message returned by [`notifications::Channel::send`] until
/// the SaaS HTTP integration is wired. Bytes-equal to the Go port's
/// `ErrNotImplemented`:
///
/// ```go
/// var ErrNotImplemented = errors.New("firefly/notificationstwilio: not yet implemented")
/// ```
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/notificationstwilio: not yet implemented";

/// Builds the not-yet-implemented sentinel as a
/// [`NotificationError::Delivery`], rendering [`ERR_NOT_IMPLEMENTED`]
/// verbatim — the analog of returning Go's `ErrNotImplemented`.
pub fn err_not_implemented() -> NotificationError {
    NotificationError::Delivery(ERR_NOT_IMPLEMENTED.to_string())
}

/// Config carries the API-key wiring needed by the production adapter.
///
/// The fields cover every wiring variable the production adapter needs; the
/// non-Twilio-flavoured fields exist because the Java module shares one
/// configuration surface across the notification provider adapters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Provider API key.
    pub api_key: String,
    /// Sender e-mail address (shared surface; unused by the SMS adapter).
    pub from_address: String,
    /// Sender phone number in E.164 form (e.g. `+15550100`).
    pub from_number: String,
    /// Twilio account SID.
    pub account_sid: String,
    /// Project identifier (shared surface; Firebase flavour).
    pub project_id: String,
    /// Server key (shared surface; Firebase flavour).
    pub server_key: String,
}

/// Channel is the placeholder [`notifications::Channel`] adapter for Twilio
/// (SMS).
///
/// Construction succeeds, [`notifications::Channel::kind`] routes
/// [`Kind::SMS`], and [`notifications::Channel::name`] answers, but
/// [`notifications::Channel::send`] returns [`err_not_implemented`] until the
/// production integration lands.
#[derive(Debug, Clone)]
pub struct Channel {
    cfg: Config,
}

impl Channel {
    /// Returns a placeholder Channel.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// The configuration this channel was constructed with, retained for the
    /// production adapter.
    pub fn config(&self) -> &Config {
        &self.cfg
    }
}

#[async_trait]
impl notifications::Channel for Channel {
    /// Implements [`notifications::Channel::kind`]; always [`Kind::SMS`].
    fn kind(&self) -> Kind {
        Kind::SMS
    }

    /// Implements [`notifications::Channel::send`]; always
    /// [`err_not_implemented`].
    async fn send(&self, _n: Notification) -> DeliveryResult {
        Err(err_not_implemented())
    }

    /// Implements [`notifications::Channel::name`].
    fn name(&self) -> String {
        "notificationstwilio-stub".to_string()
    }
}

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

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

    /// Returns `true` when `err` is the not-yet-implemented sentinel — the
    /// analog of Go's `errors.Is(err, ErrNotImplemented)`.
    fn is_not_implemented(err: &NotificationError) -> bool {
        matches!(err, NotificationError::Delivery(msg) if msg == ERR_NOT_IMPLEMENTED)
    }

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

    /// Go: `TestStubReturnsSentinel` — `Send` returns the sentinel, the name
    /// is non-empty, and the kind is set.
    #[tokio::test]
    async fn stub_returns_sentinel() {
        let c = Channel::new(Config::default());

        let err = c
            .send(Notification::default())
            .await
            .expect_err("Send: expected the sentinel error, got Ok");
        assert!(is_not_implemented(&err), "Send: {err}");

        assert!(!c.name().is_empty(), "Name should be non-empty");
        assert!(!c.kind().as_str().is_empty(), "Kind should be set");
    }

    // -------------------------------------------------------------------
    // Rust-specific additions
    // -------------------------------------------------------------------

    #[test]
    fn sentinel_message_matches_go_bytes() {
        assert_eq!(
            ERR_NOT_IMPLEMENTED,
            "firefly/notificationstwilio: not yet implemented"
        );
        let err = err_not_implemented();
        assert_eq!(
            err.to_string(),
            "firefly/notificationstwilio: not yet implemented"
        );
    }

    #[test]
    fn kind_routes_sms_and_name_matches_go() {
        let c = Channel::new(Config::default());
        assert_eq!(c.kind(), Kind::SMS);
        assert_eq!(c.kind().as_str(), "sms");
        assert_eq!(c.name(), "notificationstwilio-stub");
    }

    #[test]
    fn config_is_retained_for_the_production_adapter() {
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

    /// Registering the stub with the dispatcher routes SMS messages into it,
    /// which surface the sentinel — the consuming-code wire shape is locked
    /// before the integration ships.
    #[tokio::test]
    async fn dispatcher_routes_sms_into_stub_and_surfaces_sentinel() {
        let d = Dispatcher::new();
        d.register(Arc::new(Channel::new(Config::default())));

        let err = d
            .dispatch(Notification {
                channel: Kind::SMS,
                to: "+34911".into(),
                body: "hi".into(),
                ..Notification::default()
            })
            .await
            .expect_err("dispatch should surface the sentinel");
        assert!(is_not_implemented(&err), "dispatch: {err}");
        assert_eq!(
            err.to_string(),
            "firefly/notificationstwilio: not yet implemented"
        );

        // Other kinds stay unrouted: the stub registers only Kind::SMS.
        let err = d
            .dispatch(Notification {
                channel: Kind::EMAIL,
                ..Notification::default()
            })
            .await
            .expect_err("email has no channel");
        assert_eq!(err, NotificationError::NoChannel);
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
        assert_eq!(VERSION, "26.6.1");
    }
}
