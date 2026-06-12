# Scheduling & Notifications

Two everyday back-office needs: running work on a timer, and delivering messages
(email, SMS, push) through swappable providers. `firefly-scheduling` and
`firefly-notifications` cover them with the same port-and-adapter discipline as
the rest of the framework.

## Scheduling

`firefly-scheduling` is a `Scheduler` that owns Cron, FixedRate, and FixedDelay
triggers, runs each task on its own tokio task with panic recovery, and shuts
down gracefully on `stop()`.

> **Spring parity** — This is `@Scheduled`: cron expressions, fixed rate, fixed
> delay. The `Core` carries a `Scheduler` so you can register tasks without
> wiring one yourself.

```rust,no_run
use std::{sync::Arc, time::Duration};
use firefly_scheduling::Scheduler;

# async fn demo() {
let s = Arc::new(Scheduler::new());

// Cron: a 5-field expression (or Spring 6-field with leading seconds).
s.cron("nightly-rollup", "0 2 * * *", || async { Ok(()) }).unwrap();

// FixedRate: fire every period from a fixed anchor (slips on slow runs).
s.fixed_rate("metrics-emit", Duration::from_secs(30), || async { Ok(()) });

// FixedDelay: fire `delay` after the previous run finishes.
s.fixed_delay("cleanup", Duration::from_secs(300), || async { Ok(()) });

// Stop on a signal, then start (blocks until stop()).
let handle = Arc::clone(&s);
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.ok();
    handle.stop();
});
s.start().await; // in-flight runs finish before shutdown
# }
```

### Triggers

| Trigger             | Behaviour                                                   |
|---------------------|------------------------------------------------------------|
| `CronTrigger`       | fires when the **local** wall clock matches the expression |
| `ZonedCronTrigger`  | fires per the expression in an IANA time zone              |
| `FixedRateTrigger`  | fires every period from a fixed start anchor (slips)       |
| `FixedDelayTrigger` | fires the delay after the previous run finished            |

### Cron syntax

The canonical 5-field `minute hour day-of-month month day-of-week`, plus the
Spring **6-field** form with a leading seconds field (`sec min hour dom month
dow`), the Quartz `?` placeholder, and the `@hourly` / `@daily` / `@weekly` /
`@monthly` / `@yearly` macros. Each field accepts a literal (`15`), a list
(`0,15,30`), a range (`9-17`), a wildcard (`*`), or a step (`*/15`, `9-17/2`).
When both day-of-month and day-of-week are restricted, the rule fires when
**either** matches (Vixie cron behaviour).

For a specific time zone, use `ZonedCronTrigger`:

```rust,ignore
use firefly_scheduling::{parse_cron, ZonedCronTrigger};

let expr = parse_cron("0 9 * * MON-FRI").unwrap();
let trigger = ZonedCronTrigger::in_zone(expr, "America/New_York").unwrap();
```

Scheduled tasks surface on `/actuator/tasks`, and you can introspect cron
schedules (`CronExpr::next`, `next_n`, `seconds_until_next`) for diagnostics.

## Notifications

`firefly-notifications` defines a `Notification` envelope, a `Channel` transport
trait, and a `Dispatcher` that routes messages to channels keyed on their
`Kind`. The default `MemoryChannel` records every message (for tests); real
provider adapters slot in behind the same trait.

> **Spring parity** — The `Channel` port + `Dispatcher` is the notification
> sender abstraction; swapping `MemoryChannel` for a real provider is changing
> one registration, not your code.

```rust
use std::sync::Arc;
use firefly_notifications::{Dispatcher, Kind, MemoryChannel, Notification};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let dispatcher = Dispatcher::new();
    dispatcher.register(Arc::new(MemoryChannel::new(Kind::EMAIL)));
    dispatcher.register(Arc::new(MemoryChannel::new(Kind::SMS)));

    dispatcher
        .dispatch(Notification {
            channel: Kind::EMAIL,
            to: "alice@example.com".into(),
            subject: "Welcome".into(),
            body: "Welcome to Firefly!".into(),
            ..Notification::default()
        })
        .await
        .unwrap();
}
```

The `Dispatcher` routes by the notification's `Kind`; a message for an
unregistered kind is a `NotificationError::NoChannel`.

### Provider adapters

For production, register a real channel in place of `MemoryChannel` — same
trait, real delivery:

| Crate                          | Channel | Backing                              |
|--------------------------------|---------|--------------------------------------|
| `firefly-notifications-smtp`   | email   | `lettre` (real MIME, STARTTLS)       |
| `firefly-notifications-twilio` | SMS     | Twilio (real provider)               |
| `firefly-notifications-firebase` | push  | Firebase Cloud Messaging             |
| `firefly-notifications-sendgrid` | email | SendGrid (locked port, pending)      |
| `firefly-notifications-resend` | email   | Resend (locked port, pending)        |

```rust,ignore
use std::sync::Arc;
use firefly_notifications::{Dispatcher, Kind};

let dispatcher = Dispatcher::new();
dispatcher.register(Arc::new(smtp_email_channel)); // SmtpEmailProvider, Kind::EMAIL
dispatcher.register(Arc::new(twilio_sms_channel));  // Kind::SMS
```

Because every channel is an `Arc<dyn Channel>`, the heavy provider SDKs stay out
of services that do not select that channel — code against the `Channel` port,
register the concrete adapter at wiring time.

The next chapter covers the cache abstraction and the resilience decorators that
protect calls to the providers above. Continue to [Caching](./17-caching.md).
