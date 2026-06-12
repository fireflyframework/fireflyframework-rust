# `firefly-notifications-twilio`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** Twilio (SMS)

## Overview

`firefly-notifications-twilio` is the placeholder `Channel` adapter for Twilio
(SMS). The crate and types are declared, the port assertion compiles, and
sentinel-error smoke tests guard the wire shape — but the SaaS / cloud SDK
integration is **not yet wired**. `send` returns the not-yet-implemented
sentinel, carried through `firefly_notifications::NotificationError::Delivery`
so its rendered message is bytes-equal to the Go port's `ErrNotImplemented`:

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

## Roadmap

The real implementation is scheduled for **v26.06.x** — see the Go port's
`docs/AUDIT.md` § Roadmap for sequencing.

## Testing

```bash
cargo test -p firefly-notifications-twilio
```

Smoke tests assert (a) port satisfaction (`Channel:
firefly_notifications::Channel`, object-safe behind `Box`/`Arc`) and (b)
`send` returns the sentinel while `name` and `kind` answer non-empty. Once the
production adapter ships, these tests are deleted in favour of integration
tests against a real provider container / mock server.
