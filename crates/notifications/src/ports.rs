//! Provider and service ports for the rich notification layer (pyfly parity).
//!
//! The Rust counterpart of `pyfly.notifications.ports`. Python's
//! `runtime_checkable` `Protocol`s become object-safe `async_trait` traits so
//! that providers and services can be shared as `Arc<dyn …>` and injected
//! explicitly (replacing pyfly's DI container wiring).
//!
//! * [`EmailProvider`] / [`SmsProvider`] / [`PushProvider`] — vendor adapters
//!   that perform the actual delivery (SendGrid, Twilio, Firebase, SMTP, …).
//! * [`EmailService`] / [`SmsService`] / [`PushService`] — orchestrators that
//!   add opt-out pruning, template rendering, and metrics on top of a provider.

use async_trait::async_trait;

use crate::models::{EmailMessage, NotificationResult, PushMessage, SmsMessage};

/// A provider that delivers [`EmailMessage`]s.
///
/// Implementors must expose a stable [`name`](EmailProvider::name) (used as the
/// `provider` label on results and metrics) and a [`send`](EmailProvider::send)
/// that returns a [`NotificationResult`]. A provider may return an
/// `Err(String)` to signal a delivery failure; orchestrating services fold that
/// into a [`DeliveryStatus::Failed`](crate::models::DeliveryStatus::Failed)
/// result, matching pyfly's `_send_safely`.
#[async_trait]
pub trait EmailProvider: Send + Sync {
    /// The provider name (e.g. `"sendgrid"`, `"smtp"`, `"dummy"`).
    fn name(&self) -> &str;

    /// Delivers `message`, returning the per-send result.
    ///
    /// # Errors
    ///
    /// Returns the provider's error string when delivery fails; orchestrating
    /// services convert this into a `FAILED` [`NotificationResult`].
    async fn send(&self, message: EmailMessage) -> Result<NotificationResult, String>;
}

/// A provider that delivers [`SmsMessage`]s.
#[async_trait]
pub trait SmsProvider: Send + Sync {
    /// The provider name (e.g. `"twilio"`, `"dummy"`).
    fn name(&self) -> &str;

    /// Delivers `message`, returning the per-send result.
    ///
    /// # Errors
    ///
    /// Returns the provider's error string when delivery fails.
    async fn send(&self, message: SmsMessage) -> Result<NotificationResult, String>;
}

/// A provider that delivers [`PushMessage`]s.
#[async_trait]
pub trait PushProvider: Send + Sync {
    /// The provider name (e.g. `"firebase"`, `"dummy"`).
    fn name(&self) -> &str;

    /// Delivers `message`, returning the per-send result.
    ///
    /// # Errors
    ///
    /// Returns the provider's error string when delivery fails.
    async fn send(&self, message: PushMessage) -> Result<NotificationResult, String>;
}

/// An e-mail orchestration service.
///
/// Unlike a raw [`EmailProvider`], a service never errors back to the caller:
/// it always returns a [`NotificationResult`] whose `status` reflects the
/// outcome (`SENT` / `SUPPRESSED` / `FAILED`), matching pyfly's contract.
///
/// A *provider* failure is folded into a `FAILED` result (and increments the
/// `failed` metric) per pyfly's `_send_safely`. A local *template-render*
/// failure is also reported as `FAILED` — the service contract cannot raise —
/// but, matching pyfly (which calls `engine.render(...)` outside any
/// try/except and so never touches the `failed` counter on a render error),
/// a render failure increments **no** metric counter.
#[async_trait]
pub trait EmailService: Send + Sync {
    /// Sends `message`, returning the resulting [`NotificationResult`].
    async fn send(&self, message: EmailMessage) -> NotificationResult;
}

/// An SMS orchestration service.
#[async_trait]
pub trait SmsService: Send + Sync {
    /// Sends `message`, returning the resulting [`NotificationResult`].
    async fn send(&self, message: SmsMessage) -> NotificationResult;
}

/// A push orchestration service.
#[async_trait]
pub trait PushService: Send + Sync {
    /// Sends `message`, returning the resulting [`NotificationResult`].
    async fn send(&self, message: PushMessage) -> NotificationResult;
}
