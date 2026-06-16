# The Reactive Model — Mono & Flux

This is the keystone chapter. `firefly-reactive` is Firefly's production-grade
**reactive core**: two lazy, composable, backpressure-aware publishers — `Mono`
(0-or-1 value) and `Flux` (0..N values) — built natively on Tokio. Every
reactive surface in the framework is built from these two types: reactive HTTP
endpoints, reactive repositories, the reactive `WebClient`, and the reactive
faces of EDA and CQRS. Nothing here requires infrastructure — every example runs
in-process — so you can type each one into a scratch test and watch it pass.

No Lumen source file lands in this chapter. Instead you build the *vocabulary*
Lumen leans on twice over later: the `Flux<WalletEvent>` behind its NDJSON / SSE
*stream-a-wallet's-events* endpoint (turned on in
[Production & Deployment](./20-production.md)), and the lazy `Mono<R>` that
`Bus::send_mono` / `Bus::query_mono` return so a wallet command composes into a
reactive pipeline. Read this before the service-building chapters; everything
after it assumes you can read a `Mono`/`Flux` pipeline at a glance.

By the end of this chapter you will:

- Explain what a *reactive publisher* is, why `Mono` and `Flux` are **lazy**, and
  why their error channel is fixed to one type.
- Build, transform, combine, and run pipelines over both publishers, and read the
  `Result<Option<T>, _>` that `.block().await` returns.
- Recover from errors with `on_error_*` / `retry_backoff`, and push values
  imperatively into a `Flux` with a `FluxSink`.
- Move work between threads with a `Scheduler` (`subscribe_on` / `publish_on`).
- Turn a `Mono`/`Flux` into an HTTP response with Firefly's reactive responders
  (`MonoJson`, `NdJson`, `Sse`), and trace how Lumen's streaming endpoint uses
  them.
- See how the same two types thread through the reactive `WebClient`,
  repositories, EDA, and the CQRS bus.

## Concepts you will meet

Before the first pipeline, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — reactive publisher.** A *publisher* is a value that
> *describes* a computation producing data over time, without running it yet. You
> chain operators onto it to build a pipeline, then *subscribe* to make it run.
> The Spring analog is a Project Reactor `Publisher` (`Mono` / `Flux`), the engine
> behind Spring WebFlux. Firefly's `Mono` and `Flux` are the Rust spelling of
> exactly those.

> **Note** **Key term — lazy.** A publisher is *lazy* when building the pipeline
> does no work; the work runs only when you subscribe, block, or await. This is
> the opposite of an eagerly-executed `Future` that starts the moment it is
> created in some runtimes — a `Mono` you never subscribe to never runs.

> **Note** **Key term — backpressure.** *Backpressure* is the mechanism by which
> a slow consumer throttles a fast producer so data does not pile up in memory. A
> `Flux` honors backpressure end to end: a slow HTTP client streaming an
> `NdJson` body actually slows the producer feeding it, rather than buffering the
> whole stream up front.

## Step 1 — Meet the two publishers

Firefly's reactive core is two types, distinguished by **cardinality** — how many
values they can emit:

- **`Mono<T>`** — a producer of *at most one* value (0-or-1, plus a terminal
  error). The reactive analog of "an async function that returns a `T`."
- **`Flux<T>`** — a producer of *0..N* values plus a terminal
  completion-or-error. The reactive analog of "an async stream of `T`."

Both are **lazy**: building a pipeline does nothing; work runs only when you
subscribe, block, or await. Both are `Send + 'static`, so a `Mono` or `Flux`
drops directly into an axum handler with no wrapping.

> **Note** **Key term — terminal signal.** A pipeline ends with exactly one
> *terminal signal*: a `Flux` completes after its last value (or with no values),
> and either publisher can end early with an **error**. In `firefly-reactive` the
> error type is fixed to `firefly_kernel::FireflyError`. Fixing the error keeps
> the operator surface ergonomic — there is no error type parameter to thread
> through every `map` — and it wires straight into the framework's RFC 9457
> problem responses, so a failed pipeline becomes an `application/problem+json`
> body for free.

Type the following into a test (`#[tokio::test] async fn`) to see both shapes run
to completion:

```rust
use firefly_reactive::{Flux, Mono};

# async fn ex() {
// Mono: one value, lazily transformed, then awaited.
let n = Mono::just(20)
    .map(|x| x + 1)
    .filter(|x| *x > 10)
    .default_if_empty(0)
    .block()
    .await
    .unwrap();
assert_eq!(n, Some(21));

// Flux: a stream of values, filtered + mapped, collected to a Vec.
let xs = Flux::range(1, 5)
    .filter(|x| x % 2 == 1)
    .map(|x| x * 10)
    .collect_list()
    .block()
    .await
    .unwrap()   // Result -> Option
    .unwrap();  // Option -> Vec (collect_list always yields a list)
assert_eq!(xs, vec![10, 30, 50]);
# }
```

What just happened, block by block:

- The `Mono` pipeline starts with `Mono::just(20)`, then `map`s, `filter`s, and
  supplies a `default_if_empty(0)` in case the filter rejected the value. None of
  that ran until `.block().await`. The result is `Some(21)`: one value survived.
- The `Flux` pipeline ranges over `1..=5`, keeps the odd numbers, multiplies each
  by ten, and `collect_list` folds the whole stream into a single `Vec`. Because
  `collect_list` returns a `Mono<Vec<T>>`, running it yields `Ok(Some(vec))`.

> **Warning** `Mono::block()` is `async`: despite the name it never parks a Tokio
> worker. It resolves the publisher in place and returns
> `Result<Option<T>, FireflyError>`, so `.block().await` is the idiomatic way to
> run a pipeline to completion. The two layers it returns are deliberate — the
> outer `Result` is success-or-error, the inner `Option` is value-or-empty.

> **Tip** **Checkpoint.** Drop both snippets into a `#[tokio::test]` and run
> `cargo test`. The two `assert_eq!`s pass: `n == Some(21)` and
> `xs == vec![10, 30, 50]`. You have run your first lazy pipelines — and you have
> seen the `Result<Option<T>, _>` shape `.block().await` always returns.

### Reading the return type

Everything that runs a `Mono` to completion returns `Result<Option<T>, FireflyError>`.
The three layers each carry one fact, and reading them is a skill you will use in
every later chapter:

| Outcome                | What it means                                            |
|------------------------|----------------------------------------------------------|
| `Ok(Some(v))`          | the pipeline produced the value `v`                      |
| `Ok(None)`             | the pipeline completed **empty** (`Mono::empty`, a `filter` that rejected everything) |
| `Err(FireflyError)`    | the pipeline hit a **terminal error** and short-circuited |

A `Flux` terminal operator (`collect_list`, `reduce`, `count`, …) returns a
`Mono`, so it follows the same rule — which is why `collect_list().block().await`
unwraps twice in the example above.

## Step 2 — Create publishers

A pipeline starts at a *constructor*. You will reach for a handful constantly;
the rest are there when an edge case needs them.

`Mono` constructors:

| Constructor                       | Produces                                                |
|-----------------------------------|---------------------------------------------------------|
| `Mono::just(v)`                   | exactly `v`                                             |
| `Mono::just_or_empty(opt)`        | `v` if `Some`, empty if `None`                          |
| `Mono::empty()`                   | completes with no value (`Ok(None)`)                   |
| `Mono::error(e)`                  | a terminal error                                       |
| `Mono::from_future(fut)`          | awaits a `Future<Output = T>`                          |
| `Mono::from_result_future(fut)`   | awaits a `Future<Output = Result<T, FireflyError>>`    |
| `Mono::from_callable(f)`          | runs a `FnOnce() -> Result<Option<T>, FireflyError>` on subscribe |
| `Mono::defer(factory)`            | builds the `Mono` fresh per subscription               |

`Flux` constructors:

| Constructor                       | Produces                                       |
|-----------------------------------|------------------------------------------------|
| `Flux::just(vec)`                 | each element of the `Vec`                      |
| `Flux::from_iter(iter)`           | each element of an iterator                    |
| `Flux::range(start, count)`       | `start, start+1, …` (count items)              |
| `Flux::empty()` / `Flux::never()` | completes immediately / never emits            |
| `Flux::error(e)`                  | a terminal error                               |
| `Flux::from_stream(s)`            | a `Stream<Item = Result<T, FireflyError>>`     |
| `Flux::from_value_stream(s)`      | a `Stream<Item = T>`                           |
| `Flux::create(producer)`          | imperative push via a `FluxSink` (Step 5)      |
| `Flux::interval(period)`          | `0, 1, 2, …` on a timer                        |
| `Flux::generate(seed, step)`      | stateful generation                            |

What just happened: `Mono::just` / `Flux::just` are the literal constructors you
will use most. `from_future` / `from_result_future` are the bridge from `async`
Rust into the reactive world — the same bridge the CQRS bus uses internally to
wrap a dispatch into a `Mono`. `defer` and `from_callable` matter for **retry**,
because they build the work *fresh on each subscription* (Step 4).

> **Note** **Key term — cold publisher.** All of these are *cold*: the work is
> redone for each subscriber, starting at subscribe time, like calling a function
> again. (The opposite, a *hot* publisher, shares one running source among
> subscribers — `Mono::cache` turns a cold `Mono` into one that remembers its
> result.) Cold-by-default is what makes `retry` possible: a retry is just another
> subscription.

## Step 3 — Transform, combine, and terminate

`Mono` and `Flux` share most operator names; the differences reflect cardinality.
This is the working set — keep it nearby, you will not memorize it in one read:

| Category    | Mono                                                                 | Flux                                                                                       |
|-------------|----------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| transform   | `map` `map_async` `flat_map` `flat_map_many` `filter`                | `map` `map_async` `flat_map(n)` `concat_map` `filter` `scan` `index` `flat_map_iterable`    |
| reduce/term | `then` `then_return` `zip_with`                                       | `reduce` `collect_list` `collect_map` `count` `all` `any` `then` `last` `next` `single` `element_at` |
| limit/slice | —                                                                    | `take` `take_while` `take_last` `skip` `skip_while` `distinct` `distinct_until_changed`      |
| combine     | `when` `zip`                                                          | `merge_with` `concat_with` `zip_with` `combine_latest` `start_with` `switch_if_empty` `default_if_empty` |
| error       | `on_error_return` `on_error_resume` `on_error_map` `retry` `retry_backoff` | `on_error_resume` `on_error_continue` `retry` `retry_backoff`                          |
| time        | `timeout` `delay_element`                                            | `timeout` `delay_elements` `sample` `debounce` `interval`                                   |
| backpressure| —                                                                   | `on_backpressure_buffer` `on_backpressure_drop` `on_backpressure_latest` `limit_rate`        |
| window      | —                                                                   | `buffer` `window` `group_by`                                                                 |
| side-effect | `do_on_next` `do_on_success` `do_on_error` `do_on_finally`           | `do_on_next` `do_on_complete` `do_on_error` `do_on_finally`                                  |
| schedule    | `subscribe_on` `publish_on`                                          | `subscribe_on` `publish_on`                                                                  |
| cache/view  | `cache` `as_flux`                                                    | —                                                                                           |

The one distinction worth internalizing now is `map` versus `flat_map`. `map`
transforms each value with a plain function (`T -> U`). `flat_map` transforms each
value into *another publisher* and flattens the result — that is how you chain a
dependent reactive step onto a previous one.

```rust
use firefly_reactive::{Flux, Mono};

# async fn ex() {
// flat_map: chain a Mono onto the result of another (a sequential dependency).
let total = Mono::just(3)
    .flat_map(|seed| Mono::just(seed * 10))
    .map(|x| x + 1)
    .block()
    .await
    .unwrap();
assert_eq!(total, Some(31));

// flat_map on a Flux runs up to N inner publishers concurrently; the first
// argument is that concurrency bound.
let doubled = Flux::range(1, 3)
    .flat_map(2, |n| Mono::just(n * 2).as_flux())
    .collect_list()
    .block()
    .await
    .unwrap()
    .unwrap();
assert_eq!(doubled.len(), 3);
# }
```

What just happened:

- On the `Mono`, `flat_map(|seed| Mono::just(seed * 10))` takes the `3`, produces
  a fresh `Mono` (`30`), and flattens it so the next `map` sees `30`. This is the
  reactive spelling of "do A, then use A's result to do B."
- On the `Flux`, `flat_map(2, ..)` is the same idea fanned out: each of the three
  source values becomes an inner publisher, and up to **2** of them run at once.
  `.as_flux()` lifts the inner `Mono` into a `Flux` so the signatures line up.

To run two independent pipelines and combine their results, use `zip` (the free
function) — both run, then their outputs pair into a tuple:

```rust
use firefly_reactive::{zip, Mono};

# async fn ex() {
// zip two Monos into a tuple — both run, then combine.
let pair = zip(Mono::just("alice"), Mono::just(42))
    .block()
    .await
    .unwrap();
assert_eq!(pair, Some(("alice", 42)));
# }
```

> **Tip** **Checkpoint.** Run all three snippets in a test. You should see
> `total == Some(31)`, `doubled.len() == 3`, and `pair == Some(("alice", 42))`.
> If you reach for `flat_map` on a `Flux` and the compiler complains about
> arguments, remember the Flux form takes the concurrency bound first.

## Step 4 — Handle errors and retry

An `Err` item is **terminal** in a `Flux`: every operator short-circuits on the
first error and propagates it downstream — there is no per-element error channel.
Once an error fires, no later value flows. To recover, you choose a recovery
operator:

- `Mono::on_error_return(fallback)` — substitute a value.
- `Mono::on_error_resume(f)` / `Flux::on_error_resume(f)` — switch to a fallback
  publisher, keeping items emitted before the error.
- `Flux::on_error_continue(handler)` — drop the failing element and keep the rest
  (for operators that re-signal per item).
- `Mono::on_error_map(f)` — translate the error into a different `FireflyError`.

> **Note** **Key term — retry factory.** `retry` and `retry_backoff` cannot
> re-run an existing publisher, because a Rust stream or future is *single-use* —
> once consumed it is gone. So they take a **factory closure** that builds the
> publisher *fresh* for each attempt. Each retry is a brand-new subscription to a
> brand-new publisher. The Spring analog is Reactor's `Retry.backoff(..)`.

`Backoff::new(max_retries, base_delay)` describes the schedule. Here a flaky
source fails its first two attempts and succeeds on the third:

```rust
use std::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use firefly_reactive::{Backoff, Mono};
use firefly_kernel::FireflyError;

# async fn ex() {
let calls = Arc::new(AtomicUsize::new(0));
let c = calls.clone();
let value = Mono::retry_backoff(
    move || {
        let c = c.clone();
        Mono::from_callable(move || {
            let n = c.fetch_add(1, Ordering::SeqCst);
            if n < 2 { Err(FireflyError::internal("flaky")) } else { Ok(Some(n)) }
        })
    },
    Backoff::new(5, Duration::from_millis(10)),
)
.block()
.await
.unwrap();
assert_eq!(value, Some(2));
# }
```

What just happened: the outer `move || { … }` is the factory — `retry_backoff`
calls it once per attempt. Inside, `Mono::from_callable` runs the fallible work
when subscribed. The shared `AtomicUsize` counts attempts: calls `0` and `1`
return `Err`, so `retry_backoff` waits (10 ms, then a growing backoff) and
re-subscribes; call `2` returns `Ok(Some(2))`, which becomes the result. The
`Backoff::new(5, …)` cap means it would give up after five retries.

Deadlines are errors too. `Mono::timeout` / `Flux::timeout` map a missed deadline
to a 504 `FireflyError` (code `REACTIVE_TIMEOUT`), which renders as an RFC 9457
problem response — the same response path as any other terminal error.

> **Tip** **Checkpoint.** Run the retry snippet. It asserts `value == Some(2)`:
> the source failed twice and the third subscription succeeded. Change the
> threshold from `n < 2` to `n < 9` and watch the pipeline exhaust its five
> retries and surface the `Err` instead.

## Step 5 — Emit imperatively with `FluxSink`

The constructors in Step 2 cover declarative sources. When values arrive from a
callback, a channel, or an imperative loop, push them into a `Flux` with
`Flux::create` and a `FluxSink`.

> **Note** **Key term — `FluxSink`.** A `FluxSink` is the push handle you are
> handed inside `Flux::create`. Call `sink.next(v)` to emit a value, `sink.error(e)`
> to terminate with an error, and `sink.complete()` to finish the stream. It is
> the Rust analog of Reactor's `FluxSink` from `Flux.create(..)`.

```rust
use firefly_reactive::Flux;

# async fn ex() {
let flux = Flux::create(|sink| {
    for i in 1..=3 {
        sink.next(i);
    }
    sink.complete();
});
let out = flux.collect_list().block().await.unwrap();
assert_eq!(out, Some(vec![1, 2, 3]));
# }
```

What just happened: `Flux::create` hands your closure a `sink`. The loop emits
`1, 2, 3` with `sink.next`, then `sink.complete()` closes the stream so
`collect_list` knows it is done. This is how you adapt a non-reactive producer —
say a database cursor or a callback-based SDK — into a `Flux` without rewriting it.

> **Tip** **Checkpoint.** The test asserts `out == Some(vec![1, 2, 3])`. Forget
> the `sink.complete()` and the stream never terminates — `collect_list` would
> wait forever. Completion is your responsibility with `create`.

## Step 6 — Move work between threads with a `Scheduler`

By default a pipeline runs wherever you subscribed it. A `Scheduler` lets you move
work onto a different execution context — the Tokio worker pool, a blocking pool,
or inline — without restructuring the pipeline.

> **Note** **Key term — `Scheduler`.** A `Scheduler` decides *where* work runs.
> `Scheduler::Immediate` runs inline on the current task (no hop);
> `Scheduler::Parallel` runs on the Tokio worker pool, for CPU-bound work;
> `Scheduler::BoundedElastic` runs blocking calls on a separate pool so they never
> starve the worker pool. These mirror Reactor's `Schedulers.immediate()`,
> `.parallel()`, and `.boundedElastic()`.

Two operators apply a scheduler. `subscribe_on` hops the **source** onto a
scheduler:

```rust
use firefly_reactive::{Flux, Scheduler};

# async fn ex() {
let out = Flux::range(1, 3)
    .subscribe_on(Scheduler::Parallel)   // run the source on the Tokio worker pool
    .map(|x| x * 2)
    .collect_list()
    .block()
    .await
    .unwrap();
assert_eq!(out, Some(vec![2, 4, 6]));
# }
```

`publish_on` switches the thread for everything **downstream** of it, so the
offload point can sit anywhere in the chain — a cheap source can hop onto a worker
thread right before an expensive `map`:

```rust
use firefly_reactive::{Flux, Scheduler};

# async fn ex() {
let out = Flux::range(1, 3)
    .map(|x| x + 1)                   // runs wherever the subscribe happens
    .publish_on(Scheduler::Parallel)  // everything below hops to the worker pool
    .map(|x| x * 10)                  // runs on the Tokio worker pool
    .collect_list()
    .block()
    .await
    .unwrap();
assert_eq!(out, Some(vec![20, 30, 40]));
# }
```

What just happened: `subscribe_on` chose where the *source* runs (the whole chain
above followed it onto `Parallel`); `publish_on` split the chain in two — the
first `map` ran at the subscribe site, the second ran on the worker pool. The
rule of thumb: reach for `subscribe_on` to place a blocking or CPU-bound *source*,
and `publish_on` to offload an expensive *downstream* stage.

> **Tip** **Checkpoint.** Both snippets assert their collected results
> (`[2, 4, 6]` and `[20, 30, 40]`). The values are identical to running without a
> scheduler — schedulers change *where* work runs, never *what* it computes.

## Step 7 — Turn a publisher into an HTTP response

This is where the reactive core meets the web layer, and where Lumen will use it.
`firefly-web` ships responders that turn a `Mono`/`Flux` into an axum response: a
reactive handler simply returns one of them, and the responder drives the
publisher and writes the response. They use the stable `firefly-sse` wire format,
so any client that speaks NDJSON or SSE consumes them directly.

| Responder                | Behaviour                                                  |
|--------------------------|-----------------------------------------------------------|
| `MonoJson(Mono<T>)`      | `Ok(Some)` → 200 JSON; `Ok(None)` → 404 problem+json; `Err` → that error's RFC 9457 response |
| `NdJson(Flux<T>)`        | `application/x-ndjson`, one element per line, backpressured |
| `Sse(Flux<T>)`           | `text/event-stream`, one `data:` frame per element        |
| `SseEvents(Flux<Event>)` | `text/event-stream` with full `id` / `event` / `retry` control |

```rust,no_run
use axum::{routing::get, response::IntoResponse, Router};
use firefly_reactive::{Flux, Mono};
use firefly_web::{MonoJson, NdJson, Sse};

async fn one_order() -> impl IntoResponse {
    // Ok(Some) -> 200 application/json; Ok(None) -> 404 problem+json;
    // Err -> that error's problem response.
    MonoJson(Mono::just(serde_json::json!({ "id": "o1" })))
}

async fn stream_orders() -> impl IntoResponse {
    // application/x-ndjson, one line per element, backpressured.
    NdJson(Flux::just(vec![1, 2, 3]))
}

async fn live_orders() -> impl IntoResponse {
    // text/event-stream, one `data:` frame per element.
    Sse(Flux::just(vec![1, 2, 3]))
}

let app: Router = Router::new()
    .route("/orders/one", get(one_order))
    .route("/orders", get(stream_orders))
    .route("/orders/live", get(live_orders));
```

What just happened, responder by responder:

- **`MonoJson(Mono<T>)`** resolves the `Mono`: `Ok(Some)` → `200`
  `application/json`; `Ok(None)` → `404` `application/problem+json`; `Err` → that
  error's problem response. The empty `Mono` becoming a clean 404 is exactly the
  three-layer `Result<Option<T>, _>` from Step 1 mapped onto HTTP.
- **`NdJson(Flux<T>)`** streams `application/x-ndjson` — one compact JSON document
  plus `'\n'` per element, flushed incrementally with real backpressure. The
  `Flux`'s `Stream` is bridged straight into an axum streaming body; the whole
  stream is **never** buffered. An `Err` item mid-stream terminates the body
  cleanly.
- **`Sse(Flux<T>)`** streams `text/event-stream` — each element serialized into a
  bare `data: <json>\n\n` frame, byte-identical to the `firefly-sse` writer.
- **`SseEvents(Flux<Event>)`** streams pre-built `firefly_sse::Event` values — use
  it when you need control over the `id` / `event` / `retry` fields.

> **Warning** Backpressure here is real, not cosmetic. A slow client throttles the
> producer; nothing is buffered up front. This is what lets an `NdJson` endpoint
> stream a million rows without the response ever landing fully in memory.

### How Lumen uses this

Lumen's optional `GET /api/v1/wallets/:id/events` endpoint is exactly this shape.
It replays a wallet's persisted event stream as a `Flux<WalletEvent>` and hands it
to `NdJson` (or `Sse` with `?format=sse`). The whole handler — drawn verbatim from
`samples/lumen/src/web.rs`, feature-gated behind `streaming` — is the responders
above applied to the wallet domain:

```rust,ignore
// samples/lumen/src/web.rs — the reactive streaming handler (feature `streaming`).
#[cfg(feature = "streaming")]
async fn stream_events(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<StreamParams>,
) -> Response {
    use crate::domain::WalletEvent;
    use axum::response::IntoResponse;
    use firefly::reactive::Flux;
    use firefly::web::{NdJson, Sse};

    // `load_events` returns `Err(NotFound)` for an absent wallet, so the 404 is
    // decided before the streaming response head is committed.
    let events = match api.ledger.load_events(&id).await {
        Ok(events) => events,
        Err(e) => return WebError::from(domain_to_web(e)).into_response(),
    };
    let items: Vec<WalletEvent> = events.iter().map(WalletEvent::from_domain).collect();
    let flux = Flux::just(items);
    if params.format.as_deref() == Some("sse") {
        Sse(flux).into_response()
    } else {
        NdJson(flux).into_response()
    }
}
```

Two details worth carrying forward. First, the *not-found* decision happens
**before** the `Flux` is built, so a 404 still renders as a clean problem response
rather than a half-open stream. Second, Lumen reaches the reactive types through
the one-dependency facade — `firefly::reactive::Flux` and
`firefly::web::{NdJson, Sse}`, never the underlying `firefly-reactive` /
`firefly-web` crates. The full endpoint, including route wiring, returns in
[Production & Deployment](./20-production.md).

> **Note** Throughout the rest of the book Lumen reaches reactive types through
> the facade — `firefly::reactive::*` for `Mono`/`Flux` and `firefly::web::*` for
> the responders. The examples in *this* chapter import `firefly_reactive` /
> `firefly_web` directly so each snippet stands alone, but the two paths name the
> identical types: `firefly::reactive` re-exports `firefly_reactive`, and
> `firefly::web` re-exports `firefly_web`.

## Step 8 — Trace the same two types through the rest of the framework

`Mono` and `Flux` are not a web-only convenience; they are the spine the whole
framework hangs off. You will meet each of these in its own chapter, but seeing
the through-line now makes those chapters click.

**The reactive `WebClient`.** Firefly's reactive HTTP client hands its terminal
operators back as `Mono` / `Flux`, so an outbound call drops straight into a
reactive pipeline and composes end-to-end with the `NdJson` / `Sse` responders
above. Full treatment in [HTTP Clients](./13-http-clients.md); the shape:

```rust,no_run
use firefly_client::WebClientBuilder;
use serde::Deserialize;

#[derive(Deserialize)]
struct Order { id: String }
#[derive(Deserialize)]
struct Tick { seq: u64 }

# async fn ex() {
let client = WebClientBuilder::new("https://api.example.com").build();

// body_to_mono — the whole body decoded as one T.
let _order: firefly_reactive::Mono<Order> =
    client.get().uri("/orders/o1").retrieve().body_to_mono::<Order>();

// body_to_flux — a streamed NDJSON/SSE body decoded element-by-element,
// lazily and with backpressure.
let _ticks: firefly_reactive::Flux<Tick> = client
    .get()
    .uri("/ticks")
    .header("Accept", "application/x-ndjson")
    .retrieve()
    .body_to_flux::<Tick>();
# }
```

> **Note** The client has **no baked-in retry**. Compose `Mono::retry` /
> `Mono::retry_backoff` (Step 4) on the returned publisher, so retry policy lives
> at the call site where it belongs rather than hidden inside the client.

**Repositories.** `ReactiveCrudRepository<T, ID>` returns `Mono`/`Flux`; the SQL
adapters stream rows out of `find_all()` as a `Flux` so a huge table never lands
fully in memory. See [Persistence](./07-persistence.md).

**EDA.** `InMemoryBroker::subscribe_reactive(topic)` yields a `Flux<Event>` (in an
`EdaResult`), and `publish_mono(event)` is a cold reactive publish returning
`Mono<()>`. Lumen's ledger publishes every wallet event to a `Broker`; see
[EDA](./10-eda-messaging.md).

**CQRS.** `Bus::send_mono` / `Bus::query_mono` wrap the dispatch in a lazy
`Mono<R>`, running the *same* handler lookup and middleware chain as the
synchronous `Bus::send`. Lumen's wallet commands ride this bus; see
[CQRS](./09-cqrs.md). A taste — the shape a reactively-composed `GetWallet` query
takes (both methods take `&Arc<Bus>` so the lazy `Mono` can own the bus):

```rust,ignore
use std::sync::Arc;
use firefly::cqrs::Bus;

// `send_mono` / `query_mono` take `&Arc<Bus>` so the lazy Mono can own the bus.
let bus: Arc<Bus> = /* the WebStack's bus */;
let balance = bus
    .query_mono::<_, WalletView>(GetWallet { id: wallet_id })
    .map(|view| view.balance)
    .block()
    .await?;            // Ok(Some(<cents>))
```

> **Note** Because `firefly-reactive` fixes its error channel to `FireflyError`, a
> failed dispatch is mapped from the bus's `CqrsError` into a status-faithful
> `FireflyError` (validation → 422, authorization → 403, missing handler → 500)
> with the original error preserved as `source()` — so a reactive command flows
> straight into the RFC 9457 problem stack with no extra translation.

## Step 9 — Interop with raw `Stream` / `Future`

The reactive types are not a walled garden. Convert in and out at the edges so a
`Mono`/`Flux` can wrap (or be wrapped by) ordinary async Rust:

- **In:** `Flux::from_stream` (a `Stream<Item = Result<T, FireflyError>>`),
  `Flux::from_value_stream` (a `Stream<Item = T>`), `Mono::from_future`,
  `Mono::from_result_future`.
- **Out:** `Flux::to_stream` / `Flux::into_stream`, `Mono::into_future` (or just
  `.await` the `Mono` directly — a `Mono<T>` is itself awaitable).

What just happened: these are the seams that let you adopt the reactive core
incrementally. An existing `Stream` becomes a `Flux` you can apply backpressure
and recovery operators to; a `Mono` becomes a plain `Future` the moment some other
API wants one.

## Recap

You now hold the vocabulary the rest of the book builds on:

- **Two publishers, by cardinality.** `Mono<T>` produces 0-or-1 value;
  `Flux<T>` produces 0..N. Both are **lazy** and **cold**: nothing runs until you
  subscribe, block, or await, and each subscription redoes the work.
- **One fixed error channel.** Every terminal error is a
  `firefly_kernel::FireflyError`, which is why pipelines wire straight into RFC
  9457 problem responses with no error-type plumbing.
- **`.block().await` returns `Result<Option<T>, FireflyError>`** — outer
  success/error, inner value/empty. A `Flux` terminal operator returns a `Mono`,
  so it reads the same way.
- **Recovery is explicit.** `on_error_return` / `on_error_resume` /
  `on_error_continue` / `on_error_map` recover; `retry` / `retry_backoff` take a
  **factory** because publishers are single-use; `timeout` maps a deadline to a
  504 `FireflyError`.
- **`Flux::create` + `FluxSink`** push values imperatively; a `Scheduler`
  (`subscribe_on` / `publish_on`) moves work between inline, the worker pool, and
  the blocking pool.
- **The web responders** `MonoJson`, `NdJson`, `Sse`, and `SseEvents` turn a
  publisher into an HTTP response, with real backpressure on the streaming ones.
- **The same two types thread everywhere** — the reactive `WebClient`,
  `ReactiveCrudRepository`, the EDA broker, and `Bus::send_mono` / `query_mono`.

What this means for Lumen: no source file landed this chapter, but Lumen now has
the two publishers every reactive surface it touches is built from — the
`Flux<WalletEvent>` behind its streaming endpoint, and the `Mono<R>` behind its
command/query bus.

## Exercises

1. **Map a balance.** Build `Mono::just(1_250_i64)` (a balance in cents), `map` it
   to a major-unit `f64` (`cents as f64 / 100.0`), and `block().await` it. Confirm
   you get `Some(12.5)`.

2. **Stream wallet events as a `Flux`.** Make a `Vec<i64>` of signed balance deltas
   (`[1000, 50, -25]`), wrap it with `Flux::just`, `scan` a running balance, and
   `collect_list` it. Verify the running balances are `[1000, 1050, 1025]` — a
   hand-rolled version of what the Lumen streaming endpoint emits per event.

3. **Recover from a flaky source.** Write a `Mono::from_callable` that returns
   `Err(FireflyError::internal("flaky"))` the first two times and `Ok(Some(n))`
   afterward, then wrap it in `Mono::retry_backoff(factory, Backoff::new(5,
   Duration::from_millis(10)))`. Assert it resolves to a value — the retry-factory
   pattern Lumen's HTTP client would use against an external FX provider.

4. **Pick a responder.** Given a `Flux<WalletEvent>`, decide which responder a
   real-time dashboard wants (`Sse`) versus a bulk export (`NdJson`), and explain
   in one sentence why backpressure matters for the export case.

5. **Push, then complete.** Use `Flux::create` to emit `1..=5` with `sink.next`,
   but *omit* `sink.complete()`. Run it under a `Mono::timeout` of a few hundred
   milliseconds and observe the 504 `FireflyError` — then add the `complete()` and
   watch it pass cleanly. This is why completion is your responsibility with
   `create`.

## Where to go next

- Put these publishers to work behind real routes in
  **[Your First HTTP API](./06-first-http-api.md)** — the first chapter that
  returns a `Mono`/`Flux` from a Lumen handler.
- See `Flux` stream rows out of the database in
  **[Persistence](./07-persistence.md)** via `ReactiveCrudRepository`.
- Compose `Bus::send_mono` / `Bus::query_mono` into wallet pipelines in
  **[CQRS](./09-cqrs.md)**.
- Subscribe to a `Flux<Event>` and `publish_mono` wallet events in
  **[EDA & Messaging](./10-eda-messaging.md)**.
