# `firefly-notifications-twilio`

> **Tier:** Adapter · **Status:** Real provider (pyfly parity) + Go-parity stub · **Backing tech:** Twilio (SMS)

## Overview

`firefly-notifications-twilio` ships two layers:

* **`TwilioSmsProvider`** — the real, working HTTP integration (pyfly parity).
  It implements the `SmsProvider` port, posts to Twilio's `Messages.json`
  endpoint with HTTP basic auth and a form-encoded body, parses the response
  `sid` into a `NotificationResult`, and folds non-2xx responses into a
  `FAILED` result. See [pyfly parity](#pyfly-parity).
* **`Channel`** — the Go-parity stub `Channel` adapter, kept for backward
  compatibility. `send` returns the not-yet-implemented sentinel, carried
  through `firefly_notifications::NotificationError::Delivery` so its rendered
  message is bytes-equal to the Go port's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/notificationstwilio: not yet implemented";
```

## Why ship a stub?

* The framework's tier diagram stays correct (no missing crate).
* The port boundary stays locked — when the real implementation lands
  in v26.06, no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Quick start

```rust
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_twilio::{Channel, Config, ERR_NOT_IMPLEMENTED};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let channel = Channel::new(Config {
        account_sid: "AC0000".into(),
        api_key: "sk-test".into(),
        from_number: "+15550100".into(),
        ..Config::default()
    });

    assert_eq!(channel.kind(), Kind::SMS);
    assert_eq!(channel.name(), "notificationstwilio-stub");

    // Send returns the sentinel until the SaaS HTTP integration is wired.
    let err = channel.send(Notification::default()).await.unwrap_err();
    assert_eq!(err.to_string(), ERR_NOT_IMPLEMENTED);
}
```

The stub also slots into the `firefly_notifications::Dispatcher` exactly as
the production adapter will — it registers under `Kind::SMS` and surfaces the
sentinel from `dispatch`.

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs:
    // api_key, from_address, from_number, account_sid, project_id,
    // server_key — the configuration surface is shared across the
    // notification provider adapters, mirroring the Java module.
    // See `src/lib.rs` for the full set.
}
```

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the production adapter (API key, sender address/number, account SID, …). |
| `Channel` | Placeholder `firefly_notifications::Channel`; `Channel::new(Config)` constructs it, `Channel::config()` exposes the retained wiring, `kind()` routes `Kind::SMS`. |
| `ERR_NOT_IMPLEMENTED` | The Go-parity sentinel message. |
| `err_not_implemented()` | Builds the sentinel as `NotificationError::Delivery`. |
| `VERSION` | Framework version stamp. |

## pyfly parity

The real provider is a 1:1 port of `pyfly.notifications.providers.twilio`.

| Item | Description |
| --- | --- |
| `TwilioSmsProvider` | The working adapter. `new(account_sid, auth_token)` constructs it; `with_from_number(..)` sets a default sender; `with_base_url(..)` / `with_http_client(..)` are wiring seams (the base URL defaults to the real Twilio host). |
| `SmsProvider` | The async port (`name`, `send(SmsMessage) -> Result<NotificationResult, TwilioError>`), object-safe behind `Arc`/`Box`. |
| `SmsMessage` | Port of pyfly's `SmsMessage` (`id` defaults to a UUID v4, optional `sender`). `new(to, body)` + `with_sender(..)`. |
| `NotificationResult` | Port of pyfly's `NotificationResult` (`id`, `provider`, `status`, `provider_id`, `error`). |
| `DeliveryStatus` | Port of pyfly's `EmailStatus` enum (`QUEUED`/`SENT`/`DELIVERED`/`BOUNCED`/`FAILED`/`SUPPRESSED`). |
| `TwilioError` | `MissingSender` (no message `sender` and no provider `from_number`) and `Transport(..)`. |

### Behavior

* **Basic auth + form post:** the request goes to
  `{base}/2010-04-01/Accounts/{sid}/Messages.json` with an
  `Authorization: Basic base64(sid:token)` header and a form body of
  `From` / `To` / `Body`.
* **Sender precedence:** `SmsMessage.sender` wins over the provider's
  `from_number`.
* **Missing sender:** if neither is set, `send` returns
  `Err(TwilioError::MissingSender)` *before* any HTTP call.
* **Status mapping:** a 2xx yields `SENT` with `provider_id = sid`; any non-2xx
  yields a `FAILED` result carrying `http {status}: {body}` (the call returns
  `Ok` — non-2xx is a domain result, not a Rust error).

```rust
use firefly_notifications_twilio::{SmsMessage, SmsProvider, TwilioSmsProvider};

# async fn demo() {
let provider = TwilioSmsProvider::new("AC_sid", "tok").with_from_number("+15550001111");
let result = provider.send(SmsMessage::new("+15559876543", "hello")).await.unwrap();
assert_eq!(result.provider, "twilio");
# }
```

> **Access / secrets:** the auth token is the Twilio account auth token; this
> crate performs no token minting — pass the secret directly.

## Public surface (Go-parity stub)

The stub `Channel` continues to register with `firefly_notifications::Dispatcher`
under `Kind::SMS` and surface the `ERR_NOT_IMPLEMENTED` sentinel; consuming code
that wired the stub still compiles and behaves identically.

## Testing

```bash
cargo test -p firefly-notifications-twilio
```

The behavior tests (`tests/twilio_behavior.rs`) are ported 1:1 from pyfly's
`test_twilio_behavior.py`: they spin up an **in-process axum mock on
`127.0.0.1:0`** and assert on the actual request bytes (URL, basic-auth header,
form fields) plus the parsed `NotificationResult` — no network, no Docker. The
stub smoke tests (port satisfaction, sentinel return) are retained for Go
parity.
