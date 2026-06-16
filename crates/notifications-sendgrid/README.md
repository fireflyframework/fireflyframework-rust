# `firefly-notifications-sendgrid`

> **Tier:** Adapter · **Status:** Stable · **Backing tech:** SendGrid v3 `/mail/send` (email)

## Overview

`firefly-notifications-sendgrid` is the SendGrid (email) adapter for
`firefly_notifications::Channel`. Every operation calls SendGrid's real
[v3 `/mail/send`](https://www.twilio.com/docs/sendgrid/api-reference/mail-send/mail-send)
REST endpoint over `reqwest`; there is no stub or not-implemented sentinel.

The crate exposes two interchangeable surfaces, both backed by the same live
API:

* **`SendGridEmailProvider`** — the rich provider. It POSTs an `EmailMessage` to
  `/mail/send`, building `personalizations`, `dynamic_template_data`, and base64
  attachments, and parses the `X-Message-Id` response header.
* **`Channel` / `Config`** — the envelope adapter. It exposes a compact `Config`
  wiring surface, and `Channel::send` performs a **real** `/mail/send` call by
  mapping the channel-agnostic `Notification` envelope to an `EmailMessage`
  (using `from_address` as the sender) and delegating to
  `SendGridEmailProvider`.

```rust
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_sendgrid::{Channel, Config};

let channel = Channel::new(Config {
    api_key: "SG.xxxxx".into(),
    from_address: "noreply@example.com".into(),
    ..Config::default()
});
assert_eq!(channel.kind(), Kind::EMAIL);
assert_eq!(channel.name(), "notificationssendgrid");

// Performs a real SendGrid v3 /mail/send call (requires a live API key).
channel.send(Notification {
    channel: Kind::EMAIL,
    to: "alice@example.com".into(),
    subject: "Welcome".into(),
    body: "Welcome to Firefly!".into(),
    ..Notification::default()
}).await.unwrap();
```

## Configuration

```rust
pub struct Config {
    pub api_key: String,      // SendGrid API key ("SG.…")
    pub from_address: String, // verified sender e-mail address (used as the From)
    pub from_number: String,  // shared vendor-config field (unused here)
    pub account_sid: String,  // shared vendor-config field (unused here)
    pub project_id: String,   // shared vendor-config field (unused here)
    pub server_key: String,   // shared vendor-config field (unused here)
}
```

`api_key` and `from_address` are the only fields this adapter reads; the
remaining fields are part of the shared vendor-config surface used across
Firefly's notification adapters and are ignored here.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the adapter. |
| `Channel` | `firefly_notifications::Channel` adapter; `Channel::new(cfg)` / `Channel::with_api_base(cfg, base)` construct it, `kind()` is `Kind::EMAIL`, `name()` returns `"notificationssendgrid"`, `send()` performs a real `/mail/send` call. |
| `SendGridChannel` | Alias for `Channel`, useful where the bare name would shadow the port trait. |
| `SendGridEmailProvider` | The rich provider: `new(api_key)`, `with_api_base(api_key, base)`, `from_config(get)`. Implements `EmailProvider` and `firefly_notifications::Channel`. |
| `EmailMessage` / `Attachment` | Rich e-mail message model (to/cc/bcc, text + html, attachments, custom headers, `template_id` + `template_data`). |
| `EmailStatus` | `QUEUED` / `SENT` / `DELIVERED` / `BOUNCED` / `FAILED` / `SUPPRESSED`. |
| `NotificationResult` | `{ id, provider, status, provider_id?, error? }`. |
| `EmailProvider` | The async delivery port. |

Registering the channel in a `Dispatcher` routes `Kind::EMAIL` traffic to a real
SendGrid send:

```rust
use std::sync::Arc;

use firefly_notifications::Dispatcher;
use firefly_notifications_sendgrid::{Channel, Config};

let dispatcher = Dispatcher::new();
dispatcher.register(Arc::new(Channel::new(Config {
    api_key: "SG.xxxxx".into(),
    from_address: "noreply@example.com".into(),
    ..Config::default()
})));
```

## Behavior

* POSTs `{api_base}/mail/send` (default `https://api.sendgrid.com/v3`) with
  `Authorization: Bearer <key>` and `Content-Type: application/json`.
* Builds `personalizations[0]` from `to`/`cc`/`bcc`/`subject`; empty `cc`/`bcc`
  are dropped (SendGrid rejects null entries).
* `content` lists the `text/plain` part first, then `text/html`.
* `template_id` enables provider-native Dynamic Templates with
  `personalizations[0].dynamic_template_data`.
* Attachments are base64-encoded as `{ filename, type, content }`.
* A 2xx response → `EmailStatus::Sent` with the `X-Message-Id` header as
  `provider_id`. Any other status (or a transport error) →
  `EmailStatus::Failed` carrying `http {status}: {body}`; the rich `send` never
  returns an `Err`. The `Channel::send` envelope wrapper maps a `FAILED` result
  to `NotificationError::Delivery`.

## Testing

```bash
cargo test -p firefly-notifications-sendgrid
```

`tests/mock_send.rs` runs both surfaces against an **in-process axum mock** on
`127.0.0.1:0` (no network, no Docker) that captures the outbound request and
asserts the exact method, path, auth header, and JSON payload, then parses a
realistic response.

> **Live round trip:** A real SendGrid round trip requires a valid `SG.…` API
> key and a verified sender, which are deployment secrets. The test suite
> therefore asserts the exact wire contract against the in-process mock rather
> than calling the live SaaS; point `Channel::with_api_base` / `with_api_base`
> at `https://api.sendgrid.com/v3` with real credentials to send for real.
