# `firefly-notifications-firebase`

> **Tier:** Adapter · **Status:** Stable · **Backing tech:** Firebase Cloud Messaging HTTP v1 (push)

## Overview

`firefly-notifications-firebase` is the Firebase Cloud Messaging (push) adapter
for `firefly_notifications::Channel`. Every operation calls FCM's real
[HTTP v1 `messages:send`](https://firebase.google.com/docs/cloud-messaging/send-message)
endpoint over `reqwest`; there is no stub or not-implemented sentinel.

The crate exposes two interchangeable surfaces, both backed by the same live
API:

* **`FirebasePushProvider`** — the rich provider. It implements
  the `PushProvider` port, POSTs once per device token to
  `…/v1/projects/{id}/messages:send` with a bearer token, and folds the
  per-token outcomes into a single `NotificationResult` with partial-success
  semantics. It also exposes `send_multicast` (the explicit multi-token fan-out)
  and `send_to_topic` (FCM topic messaging).
* **`Channel` / `Config`** — the channel-agnostic envelope adapter. It provides a
  simple `Config` wiring surface, and `Channel::send` performs a **real**
  `messages:send` POST by mapping the channel-agnostic `Notification` envelope to
  a single-token `PushMessage` and delegating to `FirebasePushProvider`.

## Quick start

```rust
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_firebase::{Channel, Config};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let channel = Channel::with_access_token(
        Config { project_id: "firefly-prod".into(), ..Config::default() },
        "ya29.short-lived-oauth-token",
    );
    assert_eq!(channel.kind(), Kind::PUSH);
    assert_eq!(channel.name(), "notificationsfirebase");

    // Performs a real FCM v1 messages:send POST (requires a valid token).
    channel.send(Notification {
        channel: Kind::PUSH,
        to: "device-registration-token".into(),
        subject: "Ping".into(),
        body: "You have a new message".into(),
        ..Notification::default()
    }).await.unwrap();
}
```

The channel registers with the `firefly_notifications::Dispatcher` under
`Kind::PUSH`.

## Configuration

```rust
pub struct Config {
    pub api_key: String,      // shared vendor-config field (unused here)
    pub from_address: String, // shared vendor-config field (unused here)
    pub from_number: String,  // shared vendor-config field (unused here)
    pub account_sid: String,  // shared vendor-config field (unused here)
    pub project_id: String,   // Firebase project id (used in the messages:send URL)
    pub server_key: String,   // legacy FCM server key (NOT used by the HTTP v1 API)
}
```

Note that `server_key` is the **legacy** FCM credential and is not used
by the HTTP v1 API — supply an OAuth2 bearer token via the access-token seam
below.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the adapter (`project_id` + the shared vendor-config fields). |
| `Channel` | `firefly_notifications::Channel` adapter; `Channel::with_access_token(cfg, token)` / `Channel::with_token_provider(cfg, src)` construct it, `with_base_url(..)` is a test seam, `config()` exposes the wiring, `provider()` returns the underlying `FirebasePushProvider`, `kind()` is `Kind::PUSH`, `name()` is `"notificationsfirebase"`. |
| `FirebasePushProvider` | The rich FCM v1 provider. `new(project_id, access_token)` takes a fixed token; `with_token_provider(project_id, src)` takes a refreshing source; `with_base_url(..)` / `with_http_client(..)` are wiring seams. Adds `send_multicast(msg)` and `send_to_topic(topic, msg)`. |
| `PushProvider` | The async port (`name`, `send(PushMessage) -> Result<NotificationResult, FirebaseError>`), object-safe behind `Arc`/`Box`. |
| `AccessTokenProvider` | The token-source seam (see below); blanket-impl'd for `Fn() -> Result<String, String>`. |
| `PushMessage` | The push payload (`id` defaults to a UUID v4, `device_tokens`, `title`, `body`, `data`). `new(tokens, title, body)` + `with_data(..)`. |
| `NotificationResult` | The send outcome (`id`, `provider`, `status`, `provider_id`, `error`). |
| `DeliveryStatus` | Delivery-state enum (`QUEUED`/`SENT`/`DELIVERED`/`BOUNCED`/`FAILED`/`SUPPRESSED`). |
| `FirebaseError` | `Transport(..)` and `Token(..)`. |
| `VERSION` | Framework version stamp (`env!("CARGO_PKG_VERSION")`, e.g. `"26.6.26"`). |

## Access-token source (no JWT/OAuth here)

FCM v1 authenticates with a short-lived OAuth2 bearer token minted from a Google
**service-account** key — *not* the legacy `server_key`. **This crate
intentionally does not implement the service-account JWT → OAuth2 exchange**
(that belongs to a Google-auth library or the GCP metadata server). It accepts an
injected `AccessTokenProvider` (a `Fn() -> Result<String, String>` works),
invoked once per send so the token can refresh. Wire it to whatever
mints/refreshes tokens in your deployment (GCP metadata server,
workload-identity sidecar, `google-auth`-style library, …). Because the
`Config` has no token field, build the `Channel` with
`Channel::with_access_token` (fixed token) or `Channel::with_token_provider`
(refreshing source).

## Behavior

### Send / multicast

One HTTP send per device token, in order (FCM v1 has no native multi-token
endpoint; the deprecated batch endpoint and the Admin SDK's
`sendEachForMulticast` both loop). `send` and `send_multicast` are identical; the
aggregate result follows partial-success rules:

* **all delivered, no errors** → `SENT`, `provider_id` = `;`-joined message
  names, `error` = `None`;
* **some delivered, some failed** → `SENT` (partial success) with both
  `provider_id` and `error = "{token}: http {status}; …"` populated;
* **none delivered** → `FAILED`, `provider_id = None`.

`data` values are coerced to strings before they are sent.

### Topic messaging

`send_to_topic(topic, msg)` performs a single `messages:send` POST whose
`message.topic` field is the topic name (in place of `message.token`), per FCM v1
[topic messaging](https://firebase.google.com/docs/cloud-messaging/send-message#send_messages_to_topics).
A leading `/topics/` prefix is stripped. The message's `device_tokens` are
ignored. A non-2xx folds into a `FAILED` result carrying
`topic {name}: http {status}`.

```rust
use firefly_notifications_firebase::{FirebasePushProvider, PushMessage, PushProvider};

# async fn demo() {
let provider = FirebasePushProvider::new("my-proj", "ya29.token");
let result = provider.send(PushMessage::new(["tok-1"], "Hello", "World")).await.unwrap();
let _ = provider.send_to_topic("news", PushMessage::new(Vec::<String>::new(), "t", "b")).await;
assert_eq!(result.provider, "firebase");
# }
```

The `Channel::send` envelope wrapper maps a `FAILED` aggregate (or a token /
transport error) to `NotificationError::Delivery`.

## Testing

```bash
cargo test -p firefly-notifications-firebase
```

The behavior tests (`tests/firebase_behavior.rs`) spin up an **in-process axum
mock on `127.0.0.1:0`** that replays a per-request response queue, and assert on
the actual outbound requests (URL, bearer header, JSON payload — including the
`topic` vs `token` field) plus the parsed `NotificationResult` for the success,
error, partial-success, multicast, and topic cases — no network, no Docker.

> **Live round trip:** A real FCM round trip requires a valid OAuth2 bearer
> token minted from a service-account key — a deployment secret. The test suite
> therefore asserts the exact wire contract against the in-process mock rather
> than calling the live API; construct `Channel::with_access_token` /
> `FirebasePushProvider::new` with a real token (the default base URL is the real
> FCM host) to send for real.
