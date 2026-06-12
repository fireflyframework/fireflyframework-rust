# `firefly-notifications-twilio`

> **Tier:** Adapter · **Status:** Implemented · **Backing tech:** Twilio Programmable Messaging (SMS)

## Overview

`firefly-notifications-twilio` is the Twilio (SMS) adapter for
`firefly_notifications::Channel`. Every operation calls Twilio's real
[Programmable Messaging REST API](https://www.twilio.com/docs/sms/api/message-resource)
over `reqwest`; there is no stub or not-implemented sentinel.

The crate exposes two interchangeable surfaces, both backed by the same live
API:

* **`TwilioSmsProvider`** — the rich provider (pyfly parity). It implements the
  `SmsProvider` port, POSTs to Twilio's `Messages.json` endpoint with HTTP basic
  auth and a form-encoded body, parses the response `sid` into a
  `NotificationResult`, and folds non-2xx responses into a `FAILED` result. It
  also exposes `fetch_status`, a `GET` against the Message resource that returns
  the current delivery `MessageStatus`.
* **`Channel` / `Config`** — the Go-parity envelope adapter. It keeps the Go
  module's `Config` wiring surface, and `Channel::send` performs a **real**
  `Messages.json` POST by mapping the channel-agnostic `Notification` envelope to
  an `SmsMessage` and delegating to `TwilioSmsProvider`.

## Quick start

```rust
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_twilio::{Channel, Config};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let channel = Channel::new(Config {
        account_sid: "AC0000".into(),
        api_key: "your-auth-token".into(), // Twilio auth token
        from_number: "+15550100".into(),
        ..Config::default()
    });

    assert_eq!(channel.kind(), Kind::SMS);
    assert_eq!(channel.name(), "notificationstwilio");

    // Performs a real Twilio Messages.json POST (requires live credentials).
    channel.send(Notification {
        channel: Kind::SMS,
        to: "+15559876543".into(),
        body: "hello from Firefly".into(),
        ..Notification::default()
    }).await.unwrap();
}
```

The channel slots into the `firefly_notifications::Dispatcher` under `Kind::SMS`.

## Configuration

```rust
pub struct Config {
    pub api_key: String,      // Twilio auth token (HTTP basic-auth password)
    pub from_address: String, // shared vendor-config field (unused by SMS)
    pub from_number: String,  // default sender number, E.164 (e.g. +15550100)
    pub account_sid: String,  // Twilio account SID (basic-auth user + URL segment)
    pub project_id: String,   // shared vendor-config field (Firebase flavour)
    pub server_key: String,   // shared vendor-config field (Firebase flavour)
}
```

The shape is field-for-field identical to the Go `notificationstwilio.Config`
struct.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the adapter (account SID, auth token, sender number, …). |
| `Channel` | `firefly_notifications::Channel` adapter; `Channel::new(cfg)` / `Channel::with_base_url(cfg, base)` construct it, `config()` exposes the wiring, `provider()` returns the underlying `TwilioSmsProvider`, `kind()` routes `Kind::SMS`, `name()` is `"notificationstwilio"`. |
| `TwilioSmsProvider` | The rich provider. `new(account_sid, auth_token)` constructs it; `with_from_number(..)` sets a default sender; `with_base_url(..)` / `with_http_client(..)` are wiring seams (the base URL defaults to the real Twilio host). Adds `fetch_status(sid)`. |
| `SmsProvider` | The async port (`name`, `send(SmsMessage) -> Result<NotificationResult, TwilioError>`), object-safe behind `Arc`/`Box`. |
| `SmsMessage` | Port of pyfly's `SmsMessage` (`id` defaults to a UUID v4, optional `sender`). `new(to, body)` + `with_sender(..)`. |
| `MessageStatus` | The delivery state returned by `fetch_status` (`sid`, raw `status`, `error_code?`, `error_message?`). |
| `NotificationResult` | Port of pyfly's `NotificationResult` (`id`, `provider`, `status`, `provider_id`, `error`). |
| `DeliveryStatus` | Port of pyfly's `EmailStatus` enum (`QUEUED`/`SENT`/`DELIVERED`/`BOUNCED`/`FAILED`/`SUPPRESSED`). |
| `TwilioError` | `MissingSender`, `Transport(..)`, and `StatusFetch { status, body }`. |
| `VERSION` | Framework version stamp. |

## Behavior

### Send (matches pyfly `TwilioSmsProvider`)

* **Basic auth + form post:** the request goes to
  `{base}/2010-04-01/Accounts/{sid}/Messages.json` with an
  `Authorization: Basic base64(sid:token)` header and a form body of
  `From` / `To` / `Body`.
* **Sender precedence:** `SmsMessage.sender` wins over the provider's
  `from_number`.
* **Missing sender:** if neither is set, `send` returns
  `Err(TwilioError::MissingSender)` *before* any HTTP call. The `Channel::send`
  envelope wrapper surfaces this as `NotificationError::Delivery`.
* **Status mapping:** a 2xx yields `SENT` with `provider_id = sid`; any non-2xx
  yields a `FAILED` result carrying `http {status}: {body}` (the rich call
  returns `Ok` — non-2xx is a domain result, not a Rust error). `Channel::send`
  maps a `FAILED` result to `NotificationError::Delivery`.

### Status fetch

`TwilioSmsProvider::fetch_status(sid)` performs a `GET` against
`{base}/2010-04-01/Accounts/{sid}/Messages/{message_sid}.json` with basic auth
and parses the `status`, `error_code`, and `error_message` fields into a
`MessageStatus`. This is how callers poll for `delivered` / `failed` /
`undelivered` transitions when a status-callback webhook is not wired. A non-2xx
response returns `Err(TwilioError::StatusFetch { status, body })` (e.g. `404`
for an unknown SID).

```rust
use firefly_notifications_twilio::{SmsMessage, SmsProvider, TwilioSmsProvider};

# async fn demo() {
let provider = TwilioSmsProvider::new("AC_sid", "tok").with_from_number("+15550001111");
let result = provider.send(SmsMessage::new("+15559876543", "hello")).await.unwrap();
let status = provider.fetch_status(result.provider_id.as_deref().unwrap()).await.unwrap();
assert_eq!(result.provider, "twilio");
let _ = status.status; // e.g. "queued" / "sent" / "delivered"
# }
```

> **Access / secrets:** the auth token is the Twilio account auth token; this
> crate performs no token minting — pass the secret directly.

## Testing

```bash
cargo test -p firefly-notifications-twilio
```

The behavior tests (`tests/twilio_behavior.rs`) spin up an **in-process axum
mock on `127.0.0.1:0`** and assert on the actual request bytes (URL, basic-auth
header, form fields for send; URL, basic-auth header, and SID path for status
fetch) plus the parsed `NotificationResult` / `MessageStatus` — no network, no
Docker.

> **Live round trip:** A real Twilio round trip requires a live account SID,
> auth token, and a verified sender number — deployment secrets. The test suite
> therefore asserts the exact wire contract against the in-process mock rather
> than calling the live SaaS; construct `Channel::new` / `TwilioSmsProvider::new`
> with real credentials (the default base URL is the real Twilio host) to send
> for real.
