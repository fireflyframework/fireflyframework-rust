# `firefly-notifications-sendgrid`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** SendGrid (email)

## Overview

`firefly-notifications-sendgrid` is the placeholder
`firefly_notifications::Channel` adapter for SendGrid (email). The crate and
types are declared, the port assertion compiles, and sentinel-error smoke
tests guard the wire shape — but the SaaS / cloud SDK integration is **not
yet wired**. `send` returns the not-implemented sentinel, bytes-equal to the
Go module's `ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/notificationssendgrid: not yet implemented";
```

The sentinel travels as `firefly_notifications::NotificationError::Delivery`,
so callers match it the same way they match any transport-reported failure:

```rust
use firefly_notifications::{Channel as _, Kind, Notification};
use firefly_notifications_sendgrid::{not_implemented, Channel, Config};

let channel = Channel::new(Config::default());
assert_eq!(channel.kind(), Kind::EMAIL);
assert_eq!(channel.name(), "notificationssendgrid-stub");
assert_eq!(channel.send(Notification::default()).await.unwrap_err(), not_implemented());
```

## Why ship a stub?

* The framework's tier diagram stays correct (no missing crate).
* The port boundary stays locked — when the real implementation lands,
  no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Configuration

```rust
pub struct Config {
    pub api_key: String,      // SendGrid API key ("SG.…")
    pub from_address: String, // verified sender e-mail address
    pub from_number: String,  // shared vendor-stub field (unused here)
    pub account_sid: String,  // shared vendor-stub field (unused here)
    pub project_id: String,   // shared vendor-stub field (unused here)
    pub server_key: String,   // shared vendor-stub field (unused here)
}
```

Fields cover every wiring variable the production adapter needs; the shape is
field-for-field identical to the Go `notificationssendgrid.Config` struct.

## Public surface

| Item | Description |
| --- | --- |
| `Config` | Typed wiring for the production adapter. |
| `Channel` | Placeholder `firefly_notifications::Channel`; `Channel::new(cfg)` constructs it, `kind()` is `Kind::EMAIL`, `name()` returns `"notificationssendgrid-stub"`. |
| `SendGridChannel` | Alias for `Channel`, useful where the bare name would shadow the port trait. |
| `ERR_NOT_IMPLEMENTED` | The sentinel message, bytes-equal to Go's `ErrNotImplemented`. |
| `not_implemented()` | The sentinel as `NotificationError::Delivery`, for direct comparison. |

Registering the stub in a `Dispatcher` works exactly like the production
adapter will — only the delivery result changes when the integration ships:

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

## Roadmap

The real implementation is scheduled for a later milestone — the Go port
tracks it for **v26.06.x** in `docs/AUDIT.md` § Roadmap.

## Testing

```bash
cargo test -p firefly-notifications-sendgrid
```

Smoke tests assert (a) port satisfaction behind
`Arc<dyn firefly_notifications::Channel>` and (b) `send` returns the
not-implemented sentinel (directly and through a `Dispatcher`). Once the
production adapter ships, these tests are deleted in favour of integration
tests against a real provider container / mock server.
