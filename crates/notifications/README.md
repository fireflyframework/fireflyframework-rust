# `firefly-notifications`

> **Tier:** Adapter · **Status:** Stable

## Overview

`firefly-notifications` is the **channel-agnostic notification port**:

* `Notification` — the message envelope (channel, recipient, body,
  optional template, optional variables).
* `Channel` — the transport trait (`kind`, `send`, `name`).
* `Dispatcher` — fans messages out to channels keyed on `Kind`.
* `MemoryChannel` — a default in-process channel that records every
  message sent (useful for tests).

Concrete provider adapters (`firefly-notifications-sendgrid`,
`firefly-notifications-resend`, `firefly-notifications-twilio`,
`firefly-notifications-firebase`) live in dedicated crates and
perform real provider API calls.

## Design notes

* `Kind` is an open newtype over a string: the canonical values are the
  `Kind::EMAIL`, `Kind::SMS`, and `Kind::PUSH` constants, but custom
  transports may mint their own kinds via `Kind::new`.
* `Channel` is an `async_trait` object-safe trait (`Send + Sync`), so
  channels can be shared as `Arc<dyn Channel>` and dispatched from any
  task.
* `Dispatcher` keeps its registry behind an `RwLock`: `register`
  overwrites any previous channel for the same kind, and `dispatch`
  returns `NotificationError::NoChannel` when nothing is registered.
* The JSON shape of `Notification` omits `subject`, `template`, and
  `variables` when empty, and `created_at` marshals as RFC 3339 under
  the `createdAt` key (the default envelope stamps the zero time,
  `0001-01-01T00:00:00Z`).

## Public surface

```rust,ignore
pub struct Kind(/* open string newtype */);   // Kind::EMAIL | Kind::SMS | Kind::PUSH

pub struct Notification {
    pub id: String,
    pub channel: Kind,
    pub to: String,
    pub subject: String,                                // omitted from JSON when empty
    pub body: String,
    pub template: String,                               // omitted from JSON when empty
    pub variables: HashMap<String, serde_json::Value>,  // omitted from JSON when empty
    pub created_at: DateTime<Utc>,                      // serializes as "createdAt"
}

#[async_trait]
pub trait Channel: Send + Sync {
    fn kind(&self) -> Kind;
    async fn send(&self, n: Notification) -> DeliveryResult;
    fn name(&self) -> String;
}

pub struct Dispatcher { /* … */ }
impl Dispatcher {
    pub fn new() -> Self;
    pub fn register(&self, channel: Arc<dyn Channel>);
    pub async fn dispatch(&self, n: Notification) -> DeliveryResult; // NoChannel if Kind not registered
}

pub struct MemoryChannel { /* … */ }
impl MemoryChannel {
    pub fn new(kind: Kind) -> Self;
    pub fn messages(&self) -> Vec<Notification>;
}

pub enum NotificationError {
    NoChannel,        // "firefly/notifications: no channel registered"
    Delivery(String), // transport-reported failure
}
pub type DeliveryResult = Result<(), NotificationError>;
```

## Quick start

```rust
use std::sync::Arc;

use firefly_notifications::{Dispatcher, Kind, MemoryChannel, Notification};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let dispatcher = Dispatcher::new();
    dispatcher.register(Arc::new(MemoryChannel::new(Kind::EMAIL)));
    dispatcher.register(Arc::new(MemoryChannel::new(Kind::SMS)));

    dispatcher
        .dispatch(Notification {
            channel: Kind::EMAIL,
            to: "alice@example.com".into(),
            subject: "Welcome".into(),
            body: "Welcome to Firefly!".into(),
            ..Notification::default()
        })
        .await
        .unwrap();
}
```

For production, register `firefly_notifications_sendgrid::SendGridChannel`
instead of `MemoryChannel::new(Kind::EMAIL)` — same trait, real delivery.

## Testing

```bash
cargo test -p firefly-notifications
```

Covers dispatch routing by channel `Kind`, the `NoChannel` sentinel for
unrouted messages, register-overwrite semantics, concurrent dispatch,
and the wire format of the `Notification` JSON envelope.

## Channel-specific messaging

Alongside the envelope above (the `Notification` / `Dispatcher` /
`Channel` / `MemoryChannel` types and their wire formats), this crate
ships a richer surface for channel-specific messaging with hexagonal
provider adapters.

### What it provides

* **Rich models** — `DeliveryStatus` (`QUEUED` / `SENT` / `DELIVERED` /
  `BOUNCED` / `FAILED` / `SUPPRESSED`, serialized as the upper-case wire
  value), `Attachment`, `EmailMessage`
  (cc/bcc/attachments/custom headers/separate text+html bodies/provider-native
  template routing), `SmsMessage`, `PushMessage`, and `NotificationResult`.
* **Ports** — `EmailProvider` / `SmsProvider` / `PushProvider` (object-safe
  `async_trait`s; a provider returns `Err(String)` to signal a delivery
  failure) and `EmailService` / `SmsService` / `PushService` (which never
  error back to the caller — they always return a `NotificationResult`).
* **Default services** — `DefaultEmailService` / `DefaultSmsService` /
  `DefaultPushService`, each layering on:
  * **opt-out pruning** of EVERY recipient (`to` + `cc` + `bcc` for email,
    every device token for push), short-circuiting to a `SUPPRESSED` result
    (without calling the provider) when *all* recipients have opted out;
  * **template precedence** — an injected local engine renders into `body_html`
    and clears `template_id` / `template_data`; with no engine, those fields are
    forwarded for provider-native routing;
  * **provider-error → FAILED** conversion (safe send);
  * **metrics** — sent / failed / suppressed counters via the
    `NotificationMetrics` hook.
* **Preferences** — `PreferenceService` trait + `InMemoryPreferenceService`
  with recipient normalization (email/token lower-cased, SMS reduced to digits
  with an optional leading `+`), so opt-out matches regardless of casing or
  phone formatting.
* **Templates** — `TemplateEngine` trait, `NoOpTemplateEngine` (errors on any
  render — the safe default), and `MiniJinjaTemplateEngine` (feature
  `minijinja`, on by default) — a Jinja-compatible engine with HTML
  autoescaping always on.
* **Metrics hook** — `NotificationMetrics` trait (`record_sent` /
  `record_failed` / `record_suppressed`, with default no-op methods) +
  `InMemoryNotificationMetrics` for tests.
* **Dummy providers** — `DummyEmailProvider` / `DummySmsProvider` /
  `DummyPushProvider` (record every message, report `SENT`).
* **Config selection** — `EmailProviderSelection` / `SmsProviderSelection` /
  `PushProviderSelection` / `TemplateEngineSelection` /
  `PreferenceStoreSelection`, each with a `from_config(&str)` that maps a config
  string to a typed selection (unknown / empty falls back to the in-process
  dummy). Actual construction of vendor adapters is left to the vendor crates.

### Example

```rust
use std::sync::Arc;

use firefly_notifications::{
    DefaultEmailService, DeliveryStatus, DummyEmailProvider, EmailMessage, EmailService,
    InMemoryPreferenceService, MiniJinjaTemplateEngine,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("blocked@example.com", "email");

    let engine = Arc::new(MiniJinjaTemplateEngine::new([(
        "welcome".to_string(),
        "<h1>Hi {{ name }}</h1>".to_string(),
    )]));

    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider.clone())
        .with_preference_service(prefs)
        .with_template_engine(engine);

    let mut msg = EmailMessage::new();
    msg.to = vec!["alice@example.com".into(), "blocked@example.com".into()];
    msg.sender = "noreply@example.com".into();
    msg.subject = "Welcome".into();
    msg.template_id = Some("welcome".into());
    msg.template_data.insert("name".into(), serde_json::json!("Alice"));

    let result = service.send(msg).await;
    assert_eq!(result.status, DeliveryStatus::Sent);
    // blocked@example.com was pruned; only alice received the rendered HTML.
    assert_eq!(provider.sent()[0].to, vec!["alice@example.com".to_string()]);
    assert_eq!(provider.sent()[0].body_html.as_deref(), Some("<h1>Hi Alice</h1>"));
}
```

The channel-messaging tests live in `tests/pyfly_parity.rs` (template
rendering and per-recipient opt-out) and `tests/models_and_config.rs`
(model wire shapes and config selection).
