# `firefly-notifications-resend`

> **Tier:** Adapter · **Status:** Implemented · **Backing tech:** Resend `POST /emails` (email)

## Overview

`firefly-notifications-resend` is the Resend (email) adapter for
`firefly_notifications::Channel`. Every operation calls Resend's real
[`POST /emails`](https://resend.com/docs/api-reference/emails/send-email) REST
endpoint over `reqwest`; there is no stub or not-implemented sentinel.

The crate exposes two interchangeable surfaces, both backed by the same live
API:

* **`ResendEmailProvider`** — the rich provider (pyfly `ResendEmailProvider`).
  It POSTs an `EmailMessage` to `/emails`, supporting cc/bcc, separate text/HTML
  bodies, base64 attachments, and an optional `default_from` fallback, and
  parses the `id` field of the JSON response.
* **`Channel` / `Config`** — the Go-parity envelope adapter. It keeps the Go
  module's `Config` wiring surface, and `Channel::send` performs a **real**
  `/emails` call by mapping the channel-agnostic `Notification` envelope to an
  `EmailMessage` (using `from_address` as the sender) and delegating to
  `ResendEmailProvider`.

```rust
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_resend::{Channel, Config};

let channel = Channel::new(Config {
    api_key: "re_123".into(),
    from_address: "no-reply@example.com".into(),
    ..Config::default()
});
assert_eq!(channel.name(), "notificationsresend");

// Performs a real Resend /emails call (requires a live API key).
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
    pub api_key: String,      // Resend API key (re_…)
    pub from_address: String, // sender e-mail address (used as the From)
    pub from_number: String,  // shared vendor-config field (unused here)
    pub account_sid: String,  // shared vendor-config field (unused here)
    pub project_id: String,   // shared vendor-config field (unused here)
    pub server_key: String,   // shared vendor-config field (unused here)
}
```

The shape is field-for-field identical to the Go `notificationsresend.Config`
struct.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the adapter. |
| `Channel` | `firefly_notifications::Channel` adapter; `Channel::new(cfg)` / `Channel::with_api_base(cfg, base)` construct it, `kind()` returns `Kind::EMAIL`, `name()` returns `"notificationsresend"`, `send()` performs a real `/emails` call. |
| `ResendEmailProvider` | The rich provider: `new(api_key)`, `with_default_from(addr)`, `with_api_base(base)`, `from_config(get)`. Implements `EmailProvider` and `firefly_notifications::Channel`. |
| `EmailMessage` / `Attachment` | Rich e-mail message model (to/cc/bcc, text + html, attachments, custom headers, `template_id` + `template_data`). |
| `EmailStatus` | `QUEUED` / `SENT` / `DELIVERED` / `BOUNCED` / `FAILED` / `SUPPRESSED`. |
| `NotificationResult` | `{ id, provider, status, provider_id?, error? }`. |
| `EmailProvider` | The async delivery port. |

## Behavior (matches pyfly `ResendEmailProvider`)

* POSTs `{api_base}/emails` (default `https://api.resend.com`) with
  `Authorization: Bearer <key>` and `Content-Type: application/json`.
* `from` is `message.sender` or the configured `default_from` fallback (the
  `Channel` adapter wires `from_address` as `default_from`).
* `cc`/`bcc` are added only when non-empty; `text`/`html` only when present.
* Attachments carry `{ filename, content }` (base64) — Resend takes no `type`
  field, unlike SendGrid.
* A 2xx response → `EmailStatus::SENT` with the response JSON's `id` as
  `provider_id`. Any other status (or a transport error) →
  `EmailStatus::FAILED` carrying `http {status}: {body}`; the rich `send` never
  returns an `Err`. The `Channel::send` envelope wrapper maps a `FAILED` result
  to `NotificationError::Delivery`.

## Testing

```bash
cargo test -p firefly-notifications-resend
```

`tests/mock_send.rs` runs both surfaces against an **in-process axum mock** on
`127.0.0.1:0` (no network, no Docker) that captures the outbound request and
asserts the exact method, path, auth header, and JSON payload, then parses a
realistic response — the Rust port of
`tests/notifications/test_resend_behavior.py`.

> **Live round trip:** A real Resend round trip requires a valid `re_…` API key
> and a verified sending domain, which are deployment secrets. The test suite
> therefore asserts the exact wire contract against the in-process mock rather
> than calling the live SaaS; point `Channel::with_api_base` /
> `ResendEmailProvider::with_api_base` at `https://api.resend.com` with real
> credentials to send for real.
