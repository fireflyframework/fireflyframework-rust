# `firefly-notifications`

> **Tier:** Adapter · **Status:** Full (port + dispatcher + memory channel) · **Java original:** `firefly-notifications` · **Go module:** `notifications`

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
currently ship as port-asserting stubs, mirroring the Go port's
roadmap.

## Design notes

* The Go `type Kind string` becomes an open `Kind` newtype over a
  string: the canonical values are the `Kind::EMAIL`, `Kind::SMS`, and
  `Kind::PUSH` constants, but custom transports may mint their own
  kinds via `Kind::new`.
* Go's `Channel` interface becomes an `async_trait` object-safe trait
  (`Send + Sync`), so channels can be shared as `Arc<dyn Channel>` and
  dispatched from any task.
* `Dispatcher` keeps its registry behind an `RwLock`, matching the Go
  `sync.RWMutex` semantics: `register` overwrites any previous channel
  for the same kind, and `dispatch` returns
  `NotificationError::NoChannel` (the Go `ErrNoChannel` sentinel,
  message-for-message) when nothing is registered.
* The JSON shape of `Notification` is byte-identical to
  `json.Marshal` in Go — `subject`, `template`, and `variables` are
  omitted when empty, and `created_at` marshals as RFC 3339 under the
  `createdAt` key (the default envelope stamps the Go zero time,
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
and Go wire-format parity for the `Notification` JSON envelope.
