# `firefly-notifications-firebase`

> **Tier:** Adapter · **Status:** Real provider (pyfly parity) + Go-parity stub · **Backing tech:** Firebase Cloud Messaging (push)

## Overview

`firefly-notifications-firebase` ships two layers:

* **`FirebasePushProvider`** — the real, working FCM HTTP v1 integration (pyfly
  parity). It implements the `PushProvider` port, posts once per device token
  to `…/v1/projects/{id}/messages:send` with a bearer token, and folds the
  per-token outcomes into a single `NotificationResult` with partial-success
  semantics. See [pyfly parity](#pyfly-parity).
* **`Channel`** — the Go-parity stub `Channel` adapter (port of
  `fireflyframework-go/notificationsfirebase`), kept for backward
  compatibility. `Channel::send` returns the `ERR_NOT_IMPLEMENTED` sentinel,
  byte-for-byte equal to the Go module's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/notificationsfirebase: not yet implemented";
```

## Why ship a stub?

* The framework's tier diagram stays correct (no missing module).
* The port boundary stays locked — when the real implementation lands
  in v26.06, no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed API-key wiring for the production adapter (`api_key`, `from_address`, `from_number`, `account_sid`, `project_id`, `server_key`). |
| `Channel` | The placeholder `firefly_notifications::Channel`; `kind()` is `Kind::PUSH`, `name()` is `"notificationsfirebase-stub"`, `send()` returns the sentinel. |
| `ERR_NOT_IMPLEMENTED` | The wire-stable sentinel message, bytes-equal to the Go `ErrNotImplemented`. |
| `not_implemented()` | Builds the sentinel as a `NotificationError::Delivery`. |
| `is_not_implemented(&err)` | The analog of Go's `errors.Is(err, ErrNotImplemented)`. |
| `VERSION` | Framework version stamp (`"26.6.1"`). |

## Quick start

```rust
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_firebase::{is_not_implemented, Channel, Config};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let channel = Channel::new(Config {
        project_id: "firefly-prod".into(),
        server_key: "fcm-server-key".into(),
        ..Config::default()
    });
    assert_eq!(channel.kind(), Kind::PUSH);
    assert_eq!(channel.name(), "notificationsfirebase-stub");

    let err = channel.send(Notification::default()).await.unwrap_err();
    assert!(is_not_implemented(&err));
}
```

The channel also registers with the
[`firefly_notifications::Dispatcher`] like any other transport, so
consuming code can wire push routing today and swap in the real adapter
without changes.

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs.
    // See `src/lib.rs` for the full set.
}
```

## pyfly parity

The real provider is a 1:1 port of `pyfly.notifications.providers.firebase`.

| Item | Description |
| --- | --- |
| `FirebasePushProvider` | The working FCM v1 adapter. `new(project_id, access_token)` takes a fixed token; `with_token_provider(project_id, src)` takes a refreshing source; `with_base_url(..)` / `with_http_client(..)` are wiring seams. |
| `PushProvider` | The async port (`name`, `send(PushMessage) -> Result<NotificationResult, FirebaseError>`), object-safe behind `Arc`/`Box`. |
| `AccessTokenProvider` | The token-source seam (see below); blanket-impl'd for `Fn() -> Result<String, String>`. |
| `PushMessage` | Port of pyfly's `PushMessage` (`id` defaults to a UUID v4, `device_tokens`, `title`, `body`, `data`). `new(tokens, title, body)` + `with_data(..)`. |
| `NotificationResult` | Port of pyfly's `NotificationResult` (`id`, `provider`, `status`, `provider_id`, `error`). |
| `DeliveryStatus` | Port of pyfly's `EmailStatus` enum (`QUEUED`/`SENT`/`DELIVERED`/`BOUNCED`/`FAILED`/`SUPPRESSED`). |
| `FirebaseError` | `Transport(..)` and `Token(..)`. |

### Access-token source (no JWT/OAuth here)

FCM v1 needs a short-lived OAuth2 bearer token minted from a Google
service-account key. **This crate intentionally does not implement the
service-account JWT → OAuth2 exchange.** It accepts an injected
`AccessTokenProvider` (a `Fn() -> Result<String, String>` works), invoked once
per `send` so the token can refresh. Wire it to whatever mints/refreshes tokens
in your deployment (GCP metadata server, workload-identity sidecar,
`google-auth`-style library, …). For a fixed token use `new(..)`.

### Partial-success semantics

One HTTP send per device token, in order. The aggregate result:

* **all delivered, no errors** → `SENT`, `provider_id` = `;`-joined message
  names, `error` = `None`;
* **some delivered, some failed** → `SENT` (partial success) with both
  `provider_id` and `error = "{token}: http {status}; …"` populated;
* **none delivered** → `FAILED`, `provider_id = None`.

`data` values are coerced to strings, matching pyfly's
`{k: str(v) for k, v in message.data.items()}`.

```rust
use firefly_notifications_firebase::{FirebasePushProvider, PushMessage, PushProvider};

# async fn demo() {
let provider = FirebasePushProvider::new("my-proj", "ya29.token");
let result = provider.send(PushMessage::new(["tok-1"], "Hello", "World")).await.unwrap();
assert_eq!(result.provider, "firebase");
# }
```

## Public surface (Go-parity stub)

The stub `Channel` continues to register with `firefly_notifications::Dispatcher`
under `Kind::PUSH` and surface the `ERR_NOT_IMPLEMENTED` sentinel; consuming code
that wired the stub still compiles and behaves identically.

## Testing

```bash
cargo test -p firefly-notifications-firebase
```

The behavior tests (`tests/firebase_behavior.rs`) are ported 1:1 from pyfly's
`test_firebase_behavior.py`: they spin up an **in-process axum mock on
`127.0.0.1:0`** that replays a per-token response queue, and assert on the
actual outbound requests (URL, bearer header, JSON payload) plus the parsed
`NotificationResult` for the success, error, and partial-success cases — no
network, no Docker. The stub smoke tests (port satisfaction, sentinel return)
are retained for Go parity.
