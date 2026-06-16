# Scheduling & Notifications

A real wallet service does more than answer HTTP. It sweeps abandoned wallets
overnight, recomputes interest, retries stuck transfers, and — the part a
customer notices — emails a daily statement. None of that is triggered by a
request: it runs on a clock, or in response to something happening elsewhere.
This chapter gives Lumen its first piece of *background* work and maps the
integration surface that hangs off it.

Four framework concerns cover this back-office story, and Firefly ships each one
behind the same `firefly` facade you have depended on since
[Quickstart](./02-quickstart.md):

- **Scheduling** (`firefly-scheduling`) — running code on a timer.
- **Notifications** (`firefly-notifications`) — delivering messages through
  swappable providers (email, SMS, push).
- **Outbound webhooks** (`firefly-callbacks`) — pushing signed events to other
  systems that want to react to Lumen.
- **Inbound webhooks** (`firefly-webhooks`) — receiving and validating callbacks
  from Lumen's external payment provider.

We will build the scheduling piece end to end — a real, registered, running task
— and then map exactly where the notification, the outbound webhook, and the
inbound callback attach to it. Lumen keeps the scheduled task deliberately tiny —
it records that it ran — so you see the wiring without dragging a provider SDK
into the teaching baseline.

By the end of this chapter you will:

- Declare a scheduled task with `#[scheduled]` and understand how the framework
  *discovers* and starts it without a line of wiring in `main`.
- Tell the four trigger kinds apart — cron, zoned cron, fixed-rate, fixed-delay —
  and pick the right one for a given job.
- Read the cron grammar Firefly accepts, including the time-zone and macro forms.
- Dispatch a notification through the channel-agnostic `Dispatcher` and see how a
  real provider slots in behind the same trait.
- Sketch a signed outbound webhook with `firefly-callbacks` and a validated
  inbound webhook with `firefly-webhooks`, and know where each would hang off
  Lumen's schedule.

## Concepts you will meet

Before the first line of code, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — scheduled task.** A *scheduled task* is a piece of code
> the framework runs on a timer rather than in response to a request. You write
> the work; a *trigger* decides when it fires. The Spring analog is a method
> annotated `@Scheduled`.

> **Note** **Key term — trigger.** A *trigger* is the rule that answers "when
> does this task run next?" — every minute, at 2 a.m. daily, 30 seconds after the
> last run finished. Firefly ships four trigger kinds (cron, zoned cron,
> fixed-rate, fixed-delay); Spring expresses the same choices through
> `@Scheduled(cron=…)`, `fixedRate`, and `fixedDelay`.

> **Note** **Key term — link-time discovery.** Firefly finds your scheduled
> tasks at *link time* using the `inventory` crate: each `#[scheduled]` submits a
> registration into a compile-time registry, and the framework drains that
> registry at startup. The Spring analog is component scanning — except it
> happens at compile/link time with no runtime reflection, so "what is scheduled"
> is a fixed, inspectable set.

> **Note** **Key term — channel / dispatcher.** A *channel* is a transport that
> delivers a message (email, SMS, push); a *dispatcher* routes a message to the
> channel registered for its kind. You code against the channel *port* (a trait)
> and register a concrete provider at wiring time. Spring's analog is a
> `NotificationService` fronting pluggable senders.

## Step 1 — Declare a scheduled task

Lumen's background work lives in `src/housekeeping.rs`. The whole feature is one
zero-argument `async fn` carrying a `#[scheduled(...)]` attribute. Create the
file with this content:

```rust,ignore
// src/housekeeping.rs
use std::sync::atomic::{AtomicU64, Ordering};

use firefly::prelude::*;

/// The number of times the heartbeat has run — observable from a test (and, in
/// a real service, a counter you would surface on `/actuator/metrics`).
static HEARTBEAT_TICKS: AtomicU64 = AtomicU64::new(0);

/// A periodic housekeeping heartbeat. `#[scheduled(fixed_rate = "60s")]` makes
/// the framework call this on every tick after the initial delay.
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> {
    HEARTBEAT_TICKS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
```

Then add the module to your crate root so it is compiled and scanned. In
`src/main.rs` the line already exists in Lumen's module list (you set it up in
[Quickstart](./02-quickstart.md)); if you are following along incrementally, add
it now:

```rust,ignore
// src/main.rs
mod housekeeping;
```

What just happened, piece by piece:

- **`#[scheduled(fixed_rate = "60s", initial_delay = "5s")]`** is the whole
  declaration. `fixed_rate = "60s"` says "fire every 60 seconds"; `initial_delay
  = "5s"` says "wait 5 seconds after startup before the first fire." Durations are
  written as humanized strings (`"60s"`, `"5m"`, `"500ms"`).
- **`ledger_heartbeat` is the work.** It is a plain `async fn` taking no
  arguments and returning a `Result`. Here it just bumps an atomic counter; a real
  deployment would sweep abandoned wallets or kick off a statement run.
- **`firefly::prelude::*`** brings in everything the framework surface needs —
  including the `#[scheduled]` macro itself and the `Scheduler` type you will meet
  in Step 3. The one facade import covers it.

> **Note** **Key term — `inventory` registry.** `inventory` is the Rust crate
> Firefly uses for link-time discovery. The `#[scheduled]` macro does two things:
> it generates a `schedule_ledger_heartbeat(&scheduler)` helper, and it submits a
> `ScheduledRegistration` into the `inventory` registry. You never call the
> helper — the framework iterates the registry at boot. This is the same
> discovery mechanism that finds your controllers and CQRS handlers.

> **Tip** **Checkpoint.** `cargo build` compiles cleanly. You wrote a timer-driven
> function and registered nothing by hand — the attribute did the registration.

## Step 2 — Let the framework own the scheduler

You did not write a `tokio::spawn`, a `Scheduler::new()`, or a `start()` call —
and you will not. `FireflyApplication::run()` (the single line in Lumen's `main`)
owns the scheduler. During the boot pipeline you read about in
[Quickstart, Step 6](./02-quickstart.md#step-6--understand-what-run-does), the
framework:

1. Constructs a `Scheduler`.
2. Drains the `inventory` registry — calling
   `firefly::scheduling::register_discovered_scheduled(&scheduler)` to register
   every `#[scheduled]` task (and a sibling call for tasks declared as bean
   methods).
3. Starts the scheduler on a background tokio task, so it runs for the life of
   the process.

That means `main` never changes when you add a scheduled task — the new task is
*discovered*, not threaded through an entry point. This is the same property that
held for controllers and CQRS handlers in earlier chapters.

For testing, Lumen keeps a small helper that builds a *fresh* scheduler and runs
the same discovery against it, so a test can introspect the schedule without
booting the whole application or waiting for a tick:

```rust,ignore
// src/housekeeping.rs (continued)
/// Registers the heartbeat on a fresh scheduler and returns it — used by the
/// tests to assert it registered. `main()` does NOT call this:
/// `FireflyApplication` drains the same `inventory` registry and starts the
/// scheduler.
pub fn build_scheduler() -> std::sync::Arc<Scheduler> {
    let scheduler = std::sync::Arc::new(Scheduler::new());
    // `#[scheduled]` tasks are DISCOVERED and registered through the
    // inventory/DI registry — no manual `schedule_<fn>` calls.
    firefly::scheduling::register_discovered_scheduled(&scheduler);
    scheduler
}

/// How many heartbeat ticks have run so far.
pub fn heartbeat_ticks() -> u64 {
    HEARTBEAT_TICKS.load(Ordering::Relaxed)
}
```

What just happened: `build_scheduler` exists *only* for the tests. It calls the
exact same `register_discovered_scheduled` the framework calls, so the test
exercises real discovery. `Scheduler::new()` returns an empty scheduler whose
distributed-lock provider is a no-op (single-instance behaviour); the
registration call populates it from the `inventory` registry.

> **Note** **Key term — distributed lock.** When you run more than one copy of a
> service, you usually want a scheduled job to run on *exactly one* of them. A
> *distributed lock* (Spring/ShedLock's model) lets a task acquire a named lock
> before each tick and skip the tick if another instance holds it. The default
> `Scheduler::new()` uses a no-op lock (every tick runs), which is correct for a
> single instance; Redis- and Postgres-backed locks ship for the clustered case.

The scheduler runs each task on its own tokio task with panic recovery — a
panicking task is logged and the schedule continues — and `stop()` shuts down
gracefully, letting in-flight runs finish first. Because `run()` traps
SIGINT/SIGTERM, that graceful shutdown is wired into Lumen's lifecycle for free.

> **Tip** **Checkpoint.** Run Lumen with `cargo run` and watch the startup
> report's `scheduled tasks:` count tick up to include `ledger_heartbeat`. Five
> seconds after boot the heartbeat begins firing once a minute — silently, since
> it only bumps a counter.

## Step 3 — Observe the schedule from a test

The task is registered and ticking, but how do you *prove* it without waiting 60
seconds? Two seams make the schedule observable. First, the scheduler exposes a
snapshot of every registered task; second, the heartbeat's atomic counter records
each run. Lumen's test module asserts both:

```rust,ignore
// src/housekeeping.rs (test module)
#[cfg(test)]
mod tests {
    use super::*;

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
}
```

What just happened:

- **`scheduler.tasks()`** returns a `Vec<TaskDescriptor>` — an immutable snapshot
  taken at registration time, each entry carrying a `name`, the trigger
  descriptor, and any lock metadata. The first test introspects the schedule with
  no waiting: it registered, so its name is present.
- **`ledger_heartbeat().await`** calls the body directly. Because the work is a
  plain `async fn`, a test can invoke it without the scheduler at all and assert
  the side effect — the counter advanced by exactly one.

The same `tasks()` snapshot powers the actuator: scheduled tasks surface on
`GET /actuator/scheduledtasks` (on the management port) and in the admin
dashboard's Scheduled Tasks view, both introduced in
[Observability](./15-observability.md).

> **Tip** **Checkpoint.** `cargo test heartbeat` passes. You proved the two
> halves — the task registered, and its body runs and is observable — without a
> single `sleep` in the test.

## Step 4 — Choose the right trigger

`#[scheduled]` covers the everyday case, but the underlying `Scheduler` exposes
all four trigger kinds directly, which is what a real Lumen statement run would
use. Each kind answers "when next?" differently:

```rust,ignore
use std::{sync::Arc, time::Duration};
use firefly::prelude::Scheduler;

let s = Arc::new(Scheduler::new());

// Cron: a 5-field expression (or the 6-field form with leading seconds).
// Returns Result — the expression is parsed and can be rejected.
s.cron("daily-statements", "0 2 * * *", || async { Ok(()) }).unwrap();

// FixedRate: fire every period from a fixed anchor (the schedule slips if a run
// is slow, because the grid is anchored, not chained to the last finish).
s.fixed_rate("metrics-emit", Duration::from_secs(30), || async { Ok(()) });

// FixedDelay: fire `delay` after the previous run *finished* (no overlap).
s.fixed_delay("sweep-abandoned", Duration::from_secs(300), || async { Ok(()) });
```

What just happened: you registered three tasks on a scheduler by hand. Each
closure is a factory — the scheduler calls it once per firing to get a fresh
future — and each returns `Result<(), TaskError>`, so a failing run is logged at
`warn` and the schedule continues. Note `cron` returns a `Result` because the
expression is parsed; the rate/delay registrations take a typed `Duration` and
cannot fail.

The four kinds, and when to reach for each:

| Trigger             | Behaviour                                                   |
|---------------------|-------------------------------------------------------------|
| `CronTrigger`       | Fires when the **local** wall clock matches the expression  |
| `ZonedCronTrigger`  | Fires per the expression in an IANA time zone               |
| `FixedRateTrigger`  | Fires every period from a fixed start anchor (slips on slow runs) |
| `FixedDelayTrigger` | Fires the delay after the previous run finished             |

> **Design note.** The fixed-rate vs fixed-delay distinction is the one that bites
> people. *Fixed-rate* hangs a grid off a fixed anchor: every 30 s on the dot, so
> if one run takes 35 s, the next fires immediately (it slipped). *Fixed-delay*
> chains: it waits the delay *after* each run finishes, so a slow run pushes the
> next one out and two runs never overlap. Use fixed-rate for steady sampling
> (emit a metric every 30 s); use fixed-delay for serial work that must not pile
> up (sweep, then wait, then sweep again).

For a time zone — Lumen would run statements at 9 a.m. in the customer's region —
build a `ZonedCronTrigger` instead of relying on the host's local clock:

```rust,ignore
use firefly::scheduling::{parse_cron, ZonedCronTrigger};

// 1-5 is Monday through Friday (the day-of-week domain is 0 = Sunday .. 6 = Saturday).
let expr = parse_cron("0 9 * * 1-5").unwrap();
let trigger = ZonedCronTrigger::in_zone(expr, "America/New_York").unwrap();
```

What just happened: `parse_cron` turns the 5-field string into a typed
`CronExpr`, and `ZonedCronTrigger::in_zone` evaluates that expression in the named
IANA zone. Both calls return a `Result` — a malformed expression or an unknown
zone name is a hard error you handle at registration, never a silent
mis-schedule. To register a zoned cron task in one call, the scheduler also
offers `s.cron_in_zone(name, expr, zone, run)`.

> **Note** **Cron grammar.** Firefly's parser accepts the canonical 5-field
> expression `minute hour day-of-month month day-of-week`, an optional 6-field
> form with a leading **seconds** field, the Quartz `?` placeholder (treated as
> `*`), and the `@hourly` / `@daily` / `@weekly` / `@monthly` / `@yearly` macros.
> Day-of-week runs `0` (Sunday) through `6` (Saturday). When both day-of-month and
> day-of-week are restricted, the rule fires when **either** matches (Vixie cron
> behaviour). The `#[scheduled]` attribute accepts the same `cron = "…"` (with an
> optional `zone = "…"`) in place of `fixed_rate` / `fixed_delay`.

> **Tip** **Checkpoint.** You can name the four triggers and explain why a
> statement run uses cron (a wall-clock time), a metrics emitter uses fixed-rate
> (steady sampling), and a sweep uses fixed-delay (no overlap).

## Step 5 — Dispatch a notification

The heartbeat is the hook for outbound messaging. On a real statement tick, Lumen
would build one message per wallet and hand it to a dispatcher. The
`firefly-notifications` crate gives you a channel-agnostic `Notification`
envelope, a `Channel` transport trait, and a `Dispatcher` that routes a message
to the channel registered for its `Kind`:

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

What just happened, block by block:

- **`Dispatcher::new()`** creates an empty router. **`register`** adds a channel,
  keyed on the `Kind` it serves. Here `MemoryChannel::new(Kind::EMAIL)` is a
  built-in channel that simply records every message it receives — ideal for
  tests, and exactly what Lumen uses so the teaching baseline pulls in no provider
  SDK.
- **`dispatch`** builds a `Notification` envelope and routes it. The `channel`
  field (`Kind::EMAIL`) selects the registered channel; `to`, `subject`, and
  `body` are the message. `..Notification::default()` fills the remaining fields
  (id, template, variables, timestamp) with their zero values.
- **The `Kind` newtype** carries the canonical channels `Kind::EMAIL`,
  `Kind::SMS`, and `Kind::PUSH`, plus `Kind::new("...")` for a custom transport.
  Dispatching to a kind with no registered channel returns
  `NotificationError::NoChannel` — a typed error, not a silent drop.

> **Note** **Key term — port and adapter.** A *port* is the trait you code
> against (`Channel`); an *adapter* is a concrete implementation behind it
> (`MemoryChannel`, or a real SMTP sender). Because every channel is an
> `Arc<dyn Channel>`, the heavy provider SDKs stay out of any service that does
> not select that channel: you write your statement logic against the port and
> register the concrete adapter at wiring time. This is hexagonal architecture,
> the same shape you met in [Domain-Driven Design](./08-domain-driven-design.md).

For production, register a real channel in place of `MemoryChannel` — the same
`Channel` trait, real delivery. Each provider lives in its own crate so its SDK
only compiles into services that opt in:

| Crate                            | Channel | Backing                          |
|----------------------------------|---------|----------------------------------|
| `firefly-notifications-smtp`     | email   | `lettre` (real MIME, STARTTLS)   |
| `firefly-notifications-twilio`   | SMS     | Twilio                           |
| `firefly-notifications-firebase` | push    | Firebase Cloud Messaging         |
| `firefly-notifications-sendgrid` | email   | SendGrid                         |
| `firefly-notifications-resend`   | email   | Resend                           |

So Lumen's daily statement is, in one sentence: "on the heartbeat tick, build a
`Notification` per wallet and `dispatch` it." Swapping the in-memory channel for
a real SMTP one is a one-line registration change, never a rewrite of the
statement logic.

> **Tip** **Checkpoint.** You can trace a message from `dispatch` through the
> `Kind`-keyed routing to a channel, and you can name where a real email provider
> would slot in (the `register` call) without touching the statement code.

## Step 6 — Push an outbound webhook

When another system needs to *react* to a Lumen event — a fraud monitor that
wants every large deposit, say — Lumen pushes it an outbound webhook with
`firefly-callbacks`. Services register `Target`s; the `HmacDispatcher` signs each
payload, retries with exponential backoff, and records every `Attempt` to a
pluggable `Store` for audit:

```rust,ignore
use std::sync::Arc;
use firefly_callbacks::{CallbackEvent, DispatcherConfig, HmacDispatcher, MemoryStore};

let store = Arc::new(MemoryStore::new());
// Defaults: 3 attempts, 200 ms initial delay, doubling between retries.
let dispatcher = HmacDispatcher::new(store, DispatcherConfig::default());

// On a large deposit, Lumen would publish a CallbackEvent; the dispatcher signs
// and POSTs it to every registered Target with a stable HMAC-SHA256 signature.
```

What just happened: `HmacDispatcher::new` takes a `Store` (here the in-memory one,
which keeps every delivery attempt for inspection) and a `DispatcherConfig`. Any
field left at its zero value is filled with the default, so
`DispatcherConfig::default()` means 3 attempts with a 200 ms first delay,
doubling. On a triggering event, Lumen publishes a `CallbackEvent`; the dispatcher
POSTs the payload to each registered `Target` and records an `Attempt` row per
try, regardless of outcome.

> **Note** **Key term — HMAC signature.** HMAC (hash-based message authentication
> code) lets a receiver verify a payload came from you and was not tampered with,
> using a shared secret. Firefly signs each delivery with HMAC-SHA256 keyed on the
> target's secret, so any receiver holding the same secret can verify the request
> with a standard library call — no Firefly-specific code required.

Each delivery carries these headers, byte-identical to Firefly's Java, .NET, Go,
and Python ports, so a receiver written against any of them verifies Lumen's
deliveries unchanged:

- `X-Firefly-Event` — the event type.
- `X-Firefly-Event-Id` — the event id.
- `X-Firefly-Timestamp` — Unix seconds when the request was sent.
- `X-Firefly-Signature` — `sha256=<hmac-hex>` keyed on the target's secret.

> **Tip** **Checkpoint.** You can describe what an outbound webhook is (Lumen
> POSTing a signed event to a registered target), and name the header a receiver
> checks (`X-Firefly-Signature`).

## Step 7 — Receive an inbound webhook

The mirror image is `firefly-webhooks`: when Lumen's external payment provider
calls *back* — a charge settled, a payout failed — the inbound pipeline validates
the signature, deduplicates, and dispatches the event to a processor. Set up a
pipeline with a provider validator and mount its router:

```rust,ignore
use std::sync::Arc;
use firefly_webhooks::{web, MemoryDlq, Pipeline, StripeValidator};

let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
pipeline.register_validator(StripeValidator::new(b"whsec_test"));
let app: axum::Router = web::router(pipeline); // mount under /api/webhooks/...
```

What just happened: `Pipeline::new` takes a *dead-letter queue* — here the
in-memory `MemoryDlq`, where events that fail processing land for later
inspection. `register_validator` attaches a per-provider signature check; the
`StripeValidator` is keyed on the webhook secret (`whsec_test`). `web::router`
turns the pipeline into an `axum::Router` you mount alongside Lumen's other
routes.

> **Note** **Key term — dead-letter queue (DLQ).** A *dead-letter queue* is where
> a message goes when it cannot be processed — a validated webhook whose processor
> errored, for instance. Parking it in a DLQ instead of dropping it means you can
> inspect, fix, and replay it later. This is the same EDA pattern you met in
> [EDA & Messaging](./10-eda-messaging.md).

Validators ship for the common providers; each knows the header and algorithm its
provider uses, so registering one is the whole integration:

| Validator         | Header                  | Algorithm                                     |
|-------------------|-------------------------|-----------------------------------------------|
| `HmacValidator`   | `X-Signature` (default) | HMAC-SHA256 hex (optional `sha256=` prefix)   |
| `StripeValidator` | `Stripe-Signature`      | `t=<unix>,v1=<hmac-hex>`, 5-minute tolerance  |
| `GitHubValidator` | `X-Hub-Signature-256`   | HMAC-SHA256 hex                               |
| `TwilioValidator` | `X-Twilio-Signature`    | HMAC-SHA1 base64 of URL + sorted form fields  |

You test that receiver with the `firefly-testkit` signers — `sign_stripe`,
`sign_github`, `sign_twilio`, `sign_hmac` — which produce header values
byte-identical to what the validators expect, so a signed test request validates
exactly as a real provider's would. You will use these in
[Testing](./18-testing.md).

> **Tip** **Checkpoint.** You can name both halves of the integration story:
> outbound (`firefly-callbacks`, you sign and push) and inbound
> (`firefly-webhooks`, you validate and ingest), and point to the validator that
> matches a given provider.

## Recap — what changed in Lumen

This chapter gave Lumen its first background work and mapped its integration
surface.

| Before | After this chapter |
|--------|--------------------|
| no background work | `src/housekeeping.rs` declares `ledger_heartbeat` with `#[scheduled(fixed_rate = "60s", initial_delay = "5s")]` |
| nothing on a timer | the framework discovers and starts the task; it ticks once a minute, observable via an `AtomicU64` and `scheduler.tasks()` |
| `main` threads nothing | `main` is still the single `FireflyApplication::run()` line — the task is discovered, not wired |
| — | the heartbeat's counter is the seam where a daily-statement notification, an outbound balance-changed webhook, and an inbound provider callback would attach |

You also now know:

- That `#[scheduled]` generates a `schedule_<fn>` helper *and* submits a
  `ScheduledRegistration` to the `inventory` registry the framework drains with
  `register_discovered_scheduled(&scheduler)` — so you never hand-maintain a list
  of registration calls.
- The four trigger kinds and when each applies, plus the cron grammar Firefly
  accepts (5-field, 6-field with seconds, `?`, the `@daily`-style macros, IANA
  zones via `ZonedCronTrigger`).
- That notifications, outbound webhooks, and inbound webhooks each swap a provider
  for a one-line registration, never a code change, because every transport is a
  trait object (`Arc<dyn Channel>`, a registered `Target`, a registered
  `Validator`).

Lumen now does background work and knows exactly where its messaging hangs.

## Exercises

1. **Cron the statement run.** Replace the `fixed_rate` heartbeat with a
   `#[scheduled(cron = "0 2 * * *")]` task (or register
   `s.cron("statements", "0 2 * * *", ..)` directly on a `Scheduler`) and assert
   it appears in `scheduler.tasks()` by name — no waiting for a tick.
2. **Dispatch on the tick.** Inside `ledger_heartbeat`, build a `Dispatcher` with
   a `MemoryChannel::new(Kind::EMAIL)`, dispatch a one-line statement, and assert
   (via `MemoryChannel::messages`) that the message was recorded.
3. **Sign and receive.** Stand up a `Pipeline` with a `StripeValidator`, mount
   `web::router(..)`, and use `firefly_testkit::sign_stripe` to drive a signed
   request through it with a `TestClient` — assert it is accepted, then tamper
   with the body and assert it is rejected.
4. **Audit the outbound.** Wire an `HmacDispatcher` over a `MemoryStore`, dispatch
   a `CallbackEvent` to a `Target`, and read the recorded `Attempt` rows from the
   store to confirm the retry policy fired.
5. **Pick fixed-delay over fixed-rate.** Register a `fixed_delay` task whose body
   sleeps longer than the delay, run the scheduler briefly, and observe that runs
   never overlap — then explain why a fixed-rate task with the same period would
   have slipped instead.

## Where to go next

- Deepen the read-side cache the CQRS layer introduced in **[Caching](./17-caching.md)**.
- Revisit the actuator's `/actuator/scheduledtasks` feed and the admin
  dashboard's Scheduled Tasks view in **[Observability](./15-observability.md)**.
- Test the schedulers, dispatchers, and webhook validators with the in-memory
  channels and the `firefly-testkit` signers in **[Testing](./18-testing.md)**.
