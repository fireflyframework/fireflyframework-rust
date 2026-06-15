# Scheduling & Notifications

A real wallet service does more than answer HTTP. It sweeps abandoned wallets
overnight, recomputes interest, retries stuck transfers, and — the part a
customer notices — emails a daily statement. Two framework concerns cover that
back-office work: running code on a timer (`firefly-scheduling`) and delivering
messages through swappable providers (`firefly-notifications`), with outbound
webhooks (`firefly-callbacks`) and inbound webhooks (`firefly-webhooks`) rounding
out the integration story.

By the end of this chapter Lumen will run a **scheduled housekeeping heartbeat**,
declared with `#[scheduled]` and started from `main.rs`, and you will know
exactly where a daily-statement notification, an outbound balance-changed
webhook, and an inbound payment-provider callback would hang off it. Lumen keeps
the heartbeat deliberately tiny — it records that it ran — so the macro is shown
wired end to end without dragging in a provider SDK.

> **The back-office concerns.** `#[scheduled]` runs code on a timer (cron,
> fixed rate, fixed delay). `firefly-notifications` delivers messages through a
> `Channel` + `Dispatcher` abstraction; `firefly-callbacks` pushes signed
> outbound webhooks; `firefly-webhooks` validates and ingests inbound ones. Each
> swaps a provider for a one-line registration, never a code change.

## The scheduled heartbeat

Lumen's `src/housekeeping.rs` is the whole feature. A zero-argument `async fn`
carries `#[scheduled(...)]`; the macro generates a `schedule_<fn>(scheduler)`
registration helper. Here is the file, end to end:

```rust,ignore
use std::sync::atomic::{AtomicU64, Ordering};
use firefly::prelude::*;

/// The number of times the heartbeat has run — observable from a test (and, in
/// a real service, a counter you would surface on `/actuator/metrics`).
static HEARTBEAT_TICKS: AtomicU64 = AtomicU64::new(0);

/// A periodic housekeeping heartbeat. `#[scheduled(fixed_rate = "60s")]`
/// generates `schedule_ledger_heartbeat(scheduler)`; the framework calls this
/// on every tick after the initial delay.
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> {
    HEARTBEAT_TICKS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Registers the heartbeat on a fresh scheduler and returns it — `main()`
/// starts it; tests assert it registered.
pub fn build_scheduler() -> std::sync::Arc<Scheduler> {
    let scheduler = std::sync::Arc::new(Scheduler::new());
    schedule_ledger_heartbeat(&scheduler);
    scheduler
}

/// How many heartbeat ticks have run so far.
pub fn heartbeat_ticks() -> u64 {
    HEARTBEAT_TICKS.load(Ordering::Relaxed)
}
```

Three things are happening:

- **The macro generates the wiring.** `#[scheduled(fixed_rate = "60s",
  initial_delay = "5s")]` on `ledger_heartbeat` emits a
  `schedule_ledger_heartbeat(&scheduler)` free function. You write the work;
  the macro writes the trigger + registration. `Scheduler` comes from
  `firefly::prelude::*`, so the one facade import covers it.
- **`build_scheduler` is the composition root for timers.** It makes a fresh
  `Scheduler`, registers the heartbeat, and hands it back. `main.rs` owns when it
  starts.
- **The heartbeat is observable.** It bumps an `AtomicU64`, which a test reads —
  and which, in a real deployment, you would surface as a counter on
  `/actuator/metrics` (Chapter 15).

`main.rs` starts the scheduler on a background task, because `Scheduler::start`
runs until the scheduler is stopped:

```rust,ignore
// in main():
let scheduler = build_scheduler();
tokio::spawn(async move { scheduler.start().await });
```

`Scheduler::start` runs each task on its own tokio task with panic recovery, and
`stop()` shuts down gracefully (in-flight runs finish first). The scheduled tasks
also surface on `/actuator/scheduledtasks`.

### What the tests assert

`housekeeping.rs`'s test module proves both halves — the task registered, and it
ticks when called:

```rust,ignore
#[test]
fn scheduled_task_registers() {
    let scheduler = build_scheduler();
    let names: Vec<String> = scheduler.tasks().into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"ledger_heartbeat".to_string()));
}

#[tokio::test]
async fn heartbeat_runs() {
    let before = heartbeat_ticks();
    ledger_heartbeat().await.unwrap();
    assert_eq!(heartbeat_ticks(), before + 1);
}
```

`scheduler.tasks()` returns a `Vec<TaskDescriptor>` (each has a `name`), so a test
can introspect the schedule without waiting for a tick — and the dashboard's
Scheduled Tasks view (Chapter 15) reads the same list.

## The trigger menu

`#[scheduled]` covers the everyday case; the underlying `Scheduler` exposes all
four trigger kinds directly, which is what a real Lumen statement run would use:

```rust,ignore
use std::{sync::Arc, time::Duration};
use firefly::prelude::Scheduler;

let s = Arc::new(Scheduler::new());

// Cron: a 5-field expression (or the 6-field form with leading seconds).
s.cron("daily-statements", "0 2 * * *", || async { Ok(()) }).unwrap();

// FixedRate: fire every period from a fixed anchor (slips on a slow run).
s.fixed_rate("metrics-emit", Duration::from_secs(30), || async { Ok(()) });

// FixedDelay: fire `delay` after the previous run finished.
s.fixed_delay("sweep-abandoned", Duration::from_secs(300), || async { Ok(()) });
```

| Trigger             | Behavior                                                    |
|---------------------|-------------------------------------------------------------|
| `CronTrigger`       | fires when the **local** wall clock matches the expression  |
| `ZonedCronTrigger`  | fires per the expression in an IANA time zone               |
| `FixedRateTrigger`  | fires every period from a fixed start anchor (slips)        |
| `FixedDelayTrigger` | fires the delay after the previous run finished             |

The cron grammar is the canonical 5-field `minute hour day-of-month month
day-of-week`, plus an optional 6-field form with leading seconds, the `?`
placeholder, and the `@daily` / `@hourly` / `@weekly` macros. For a time zone,
build a `ZonedCronTrigger`:

```rust,ignore
use firefly::scheduling::{parse_cron, ZonedCronTrigger};

// Lumen would run statements at 9am in the customer's region.
// 1-5 is Monday through Friday (the dow domain is 0=Sunday .. 6=Saturday).
let expr = parse_cron("0 9 * * 1-5").unwrap();
let trigger = ZonedCronTrigger::in_zone(expr, "America/New_York").unwrap();
```

> **Cron grammar.** Firefly's cron parser accepts the canonical 5-field
> expression, an optional 6-field leading-seconds form, the `?` placeholder, and
> the `@daily` / `@hourly` / `@weekly` macros. `#[scheduled]` generates the
> registration helper (`schedule_<fn>`) so you write only the work function.

## Notifications — where the daily statement hangs

The heartbeat is the hook for outbound messaging. `firefly-notifications` gives
you a channel-agnostic `Notification` envelope, a `Channel` transport trait, and
a `Dispatcher` that routes a message to the channel registered for its `Kind`.
The default `MemoryChannel` records every message (ideal for tests); a real
provider adapter slots in behind the same trait. A Lumen statement run, in
sketch:

```rust,ignore
use std::sync::Arc;
use firefly_notifications::{Dispatcher, Kind, MemoryChannel, Notification};

let dispatcher = Dispatcher::new();
dispatcher.register(Arc::new(MemoryChannel::new(Kind::EMAIL)));

dispatcher
    .dispatch(Notification {
        channel: Kind::EMAIL,
        to: "alice@example.com".into(),
        subject: "Your Lumen statement".into(),
        body: "Closing balance: $42.00".into(),
        ..Notification::default()
    })
    .await
    .unwrap();
```

The `Dispatcher` routes by `Kind` (`Kind::EMAIL` / `Kind::SMS` / `Kind::PUSH`, or
a custom `Kind::new("...")`); a message for an unregistered kind is a
`NotificationError::NoChannel`. For production, register a real channel in place
of `MemoryChannel` — same trait, real delivery:

| Crate                            | Channel | Backing                          |
|----------------------------------|---------|----------------------------------|
| `firefly-notifications-smtp`     | email   | `lettre` (real MIME, STARTTLS)   |
| `firefly-notifications-twilio`   | SMS     | Twilio                           |
| `firefly-notifications-firebase` | push    | Firebase Cloud Messaging         |
| `firefly-notifications-sendgrid` | email   | SendGrid                         |
| `firefly-notifications-resend`   | email   | Resend                           |

Because every channel is an `Arc<dyn Channel>`, the heavy provider SDKs stay out
of a service that does not select that channel: code against the `Channel` port,
register the concrete adapter at wiring time. So Lumen's daily statement is "on
the heartbeat tick, build a `Notification` per wallet and `dispatch` it" — the
provider is a wiring decision, not a rewrite.

## Outbound webhooks — telling other systems a balance changed

When another system needs to *react* to a Lumen event — a fraud monitor that
wants every large deposit, say — you push it an outbound webhook with
`firefly-callbacks`. Services register `Target`s; the `HmacDispatcher` signs each
payload with HMAC-SHA256, retries with exponential backoff, and records every
`Attempt` to a pluggable `Store` for audit:

```rust,ignore
use std::sync::Arc;
use firefly_callbacks::{CallbackEvent, DispatcherConfig, HmacDispatcher, MemoryStore};

let store = Arc::new(MemoryStore::new());
let dispatcher = HmacDispatcher::new(store, DispatcherConfig::default()); // 3 attempts, 200ms, doubling

// On a large deposit, Lumen would publish a CallbackEvent; the dispatcher signs
// and POSTs it to every registered Target with a stable HMAC-SHA256 signature.
```

Each delivery carries `X-Firefly-Event`, `X-Firefly-Event-Id`,
`X-Firefly-Timestamp`, and an `X-Firefly-Signature: sha256=<hmac-hex>` keyed on
the target's secret, so any receiver that knows the shared secret verifies
Lumen's deliveries with a standard HMAC check.

## Inbound webhooks — receiving a provider callback

The mirror image is `firefly-webhooks`: when Lumen's external payment provider
calls *back* (a charge settled, a payout failed), the inbound pipeline validates
the signature, deduplicates, and dispatches to a processor. The signature
validators ship for the common providers:

| Validator         | Header                  | Algorithm                                     |
|-------------------|-------------------------|-----------------------------------------------|
| `HmacValidator`   | `X-Signature` (default) | HMAC-SHA256 hex (optional `sha256=` prefix)   |
| `StripeValidator` | `Stripe-Signature`      | `t=<unix>,v1=<hmac-hex>`, 5-minute tolerance  |
| `GitHubValidator` | `X-Hub-Signature-256`   | HMAC-SHA256 hex                               |
| `TwilioValidator` | `X-Twilio-Signature`    | HMAC-SHA1 base64 of URL + sorted form fields  |

```rust,ignore
use std::sync::Arc;
use firefly_webhooks::{web, MemoryDlq, Pipeline, StripeValidator};

let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
pipeline.register_validator(StripeValidator::new(b"whsec_test"));
let app: axum::Router = web::router(pipeline); // mount under /api/webhooks/...
```

You test that receiver with the `firefly-testkit` signers — `sign_stripe`,
`sign_github`, `sign_twilio`, `sign_hmac` — which produce header values
byte-identical to what the validators expect, so a signed test request validates
exactly as a real provider's would (Chapter 18).

## What changed in Lumen

This chapter gave Lumen its first background work and mapped its integration
surface:

- **`src/housekeeping.rs`** declares the `ledger_heartbeat` with
  `#[scheduled(fixed_rate = "60s", initial_delay = "5s")]`; the macro generates
  `schedule_ledger_heartbeat(&scheduler)`, and `build_scheduler` registers it on
  a fresh `Scheduler`.
- **`src/main.rs`** starts the scheduler on a background tokio task
  (`tokio::spawn(async move { scheduler.start().await })`); the tasks surface on
  `/actuator/scheduledtasks` and in the dashboard's Scheduled Tasks view.
- The heartbeat's `AtomicU64` is the seam where a real **daily-statement
  notification** (`firefly-notifications`), an **outbound balance-changed
  webhook** (`firefly-callbacks`), and an **inbound provider callback**
  (`firefly-webhooks`) would attach — each a registration, not a rewrite.

## Exercises

1. **Cron the statement run.** Replace the `fixed_rate` heartbeat with a
   `#[scheduled(cron = "0 2 * * *")]` task (or register `s.cron("statements",
   "0 2 * * *", ..)` directly) and assert it appears in `scheduler.tasks()` by
   name.
2. **Dispatch on the tick.** Inside `ledger_heartbeat`, build a `Dispatcher` with
   a `MemoryChannel::new(Kind::EMAIL)`, dispatch a one-line statement, and assert
   (via `MemoryChannel::messages`) that the message was recorded.
3. **Sign and receive.** Stand up a `Pipeline` with a `StripeValidator`, mount
   `web::router(..)`, and use `firefly_testkit::sign_stripe` to drive a signed
   request through it with a `TestClient` — assert it is accepted, then tamper
   with the body and assert it is rejected.
4. **Audit the outbound.** Wire an `HmacDispatcher` over a `MemoryStore`,
   dispatch a `CallbackEvent` to a `Target`, and read the recorded `Attempt`
   rows from the store to confirm the retry policy fired.

Lumen now does background work and knows where its messaging hangs. The next
chapter deepens the read-side cache the CQRS layer introduced. Continue to
[Caching](./17-caching.md).
