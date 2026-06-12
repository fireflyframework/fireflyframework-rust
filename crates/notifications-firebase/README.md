# `firefly-notifications-firebase`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** Firebase Cloud Messaging (push)

## Overview

`firefly-notifications-firebase` is the placeholder
[`firefly_notifications::Channel`] adapter for Firebase Cloud Messaging
(push) — the Rust port of the Go module
`fireflyframework-go/notificationsfirebase`. The crate and types are
declared, the port assertion compiles, and sentinel-error smoke tests
guard the wire shape — but the SaaS / cloud SDK integration is **not yet
wired**. `Channel::send` returns the `ERR_NOT_IMPLEMENTED` sentinel,
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

## Roadmap

The real implementation is scheduled for **v26.06.x**, mirroring the Go
module's roadmap (see the Go repo's `docs/AUDIT.md` § Roadmap for
sequencing).

## Testing

```bash
cargo test -p firefly-notifications-firebase
```

Smoke tests assert (a) port satisfaction (the adapter coerces to
`Box<dyn firefly_notifications::Channel>` / `Arc<dyn ...>`) and (b)
`send` returns `ERR_NOT_IMPLEMENTED` while `name`/`kind` are non-empty —
the 1:1 port of the Go `TestImplementsPort` and `TestStubReturnsSentinel`
cases — plus Rust-specific coverage for sentinel byte-parity, error
taxonomy, config plumbing, dispatcher wiring, and `Send + Sync` bounds.
Once the production adapter ships, these tests are deleted in favour of
integration tests against a real provider container / mock server.
