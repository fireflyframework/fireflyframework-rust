# `firefly-notifications-smtp`

> **Tier:** Adapter · **Status:** Stable · **Backing tech:** SMTP (via [`lettre`](https://docs.rs/lettre))

## Overview

`firefly-notifications-smtp` is a real SMTP e-mail provider. It builds a
standards-compliant MIME message from a rich `EmailMessage` and delivers it
over SMTP with optional STARTTLS and SMTP AUTH.

```rust
use firefly_notifications_smtp::{EmailMessage, EmailProvider, SmtpConfig, SmtpEmailProvider};

let provider = SmtpEmailProvider::new(SmtpConfig {
    host: "smtp.example.com".into(),
    port: 587,
    username: Some("apikey".into()),
    password: Some("secret".into()),
    use_tls: true,
});

let msg = EmailMessage {
    to: vec!["dest@example.com".into()],
    cc: vec!["carbon@example.com".into()],
    bcc: vec!["hidden@example.com".into()],
    sender: "from@example.com".into(),
    subject: "Hello SMTP".into(),
    body_text: Some("plain text body".into()),
    body_html: Some("<h1>Hello</h1>".into()),
    ..EmailMessage::default()
};
let result = provider.send(msg).await; // NotificationResult { status: SENT | FAILED, .. }
```

## Behavior

* MIME structure is assembled from the `EmailMessage` contents:
  * text only → a single `text/plain` part;
  * HTML only → a single `text/html` part;
  * text + HTML → a `multipart/alternative` (plain first, then HTML);
  * any of the above plus attachments → a `multipart/mixed` wrapping the body
    followed by one attachment part each.
* **BCC is delivered but never leaked**: bcc recipients are in the SMTP
  envelope (so the server delivers to them) but lettre strips the `Bcc` header
  from the formatted message, so other recipients never see them.
* **Custom headers** are added verbatim, except names that collide
  (case-insensitively) with the reserved `From`/`To`/`Cc`/`Bcc`/`Subject`,
  which are ignored so they cannot clobber the standard headers.
* STARTTLS is used when `use_tls` is set; credentials are attached only when
  both username and password are present.
* Any failure (bad address, connection error, server rejection) is folded into
  `EmailStatus::FAILED` carrying the error text — `send` never returns an
  `Err`.

## Public surface

| Item | Description |
| --- | --- |
| `SmtpEmailProvider` | The provider: `new(cfg)`, `from_config(get)`. Implements `EmailProvider` and a thin `firefly_notifications::Channel` (`name()` = `"notificationssmtp"`). |
| `SmtpConfig` | `{ host, port, username?, password?, use_tls }`; defaults to port 587 with STARTTLS on. `from_config(get)` parses flat config keys. |
| `build_message(&EmailMessage)` | The pure message-builder. Returns the exact `lettre::Message` the provider would transmit — used by the structure tests. |
| `EmailMessage` / `Attachment` | Rich e-mail message model. |
| `EmailStatus` | `QUEUED` / `SENT` / `DELIVERED` / `BOUNCED` / `FAILED` / `SUPPRESSED`. |
| `NotificationResult` | `{ id, provider, status, provider_id?, error? }`. |
| `EmailProvider` | The async delivery port. |
| `BuildError` | Errors from `build_message` (invalid address/header, assembly). |

## Testing

```bash
cargo test -p firefly-notifications-smtp
```

Per the framework's bare-machine test policy the unit suite needs **no live
SMTP server**. `build_message` is a pure function over `EmailMessage`; the
tests assert the resulting `lettre::Message` — its headers, MIME parts,
envelope recipients, and the bcc-not-leaked invariant — directly.
Connection-failure mapping is exercised against a closed port.

A genuine end-to-end send lives in `tests/smtp_integration.rs` as an
**env-gated** integration test (no `#[ignore]`): with `FIREFLY_TEST_SMTP_ADDR`
unset it prints `skipping …` and returns, so `cargo test` stays green; with it
set (e.g. `localhost:1026`, MailHog) it delivers a real e-mail and verifies it
arrived via the MailHog HTTP API.

```bash
export FIREFLY_TEST_SMTP_ADDR="localhost:1026"
cargo test -p firefly-notifications-smtp --test smtp_integration
```
