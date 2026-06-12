# The Reactive Model — Mono & Flux

This is the keystone chapter. `firefly-reactive` is a faithful,
production-grade **Reactor / WebFlux-style reactive core** — the Rust analog of
Project Reactor's `Mono` and `Flux`, the engine behind Spring WebFlux and the
Java Firefly framework. Every reactive surface in the framework — reactive HTTP
endpoints, reactive repositories, the reactive `WebClient`, reactive EDA and
CQRS — is built on the two types you will learn here. Read this before the
service-building chapters.

## Mono and Flux

Two publishers, mirroring Reactor exactly:

- **`Mono<T>`** — a producer of *at most one* value (0-or-1, plus a terminal
  error). The reactive analog of "an async function that returns a `T`."
- **`Flux<T>`** — a producer of *0..N* values plus a terminal
  completion-or-error. The reactive analog of "an async stream of `T`."

Both are **lazy**: building a pipeline does nothing; work runs only when you
subscribe, block, or await. Both are `Send + 'static`, so a `Mono` or `Flux`
drops directly into an axum handler.

The error type is fixed to `firefly_kernel::FireflyError`, exactly as WebFlux
models everything as a `Throwable`. Fixing the error keeps the operator surface
ergonomic (no error type parameter) and wires straight into the framework's
RFC 7807 problem responses.

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

> **Reactor parity** — `Mono::block()` here is *not* the thread-parking
> `Mono.block()` of the JVM. It is `async` and never parks a Tokio worker; it
> resolves the publisher in place and returns `Result<Option<T>, FireflyError>`.

## Reactor → firefly-reactive concept map

| Project Reactor                       | firefly-reactive                                |
|---------------------------------------|-------------------------------------------------|
| `Mono<T>`                             | `Mono<T>`                                        |
| `Flux<T>`                             | `Flux<T>`                                        |
| `Throwable` (error signal)            | `firefly_kernel::FireflyError` (fixed)           |
| `Mono.empty()` / `onComplete`         | `Ok(None)` from a `Mono`                          |
| `onError(t)`                          | `Err(FireflyError)` (terminal)                    |
| `Mono.block()`                        | `Mono::block` (async — never parks a thread)      |
| `Mono.subscribe(..)` / `Flux.subscribe(..)` | `Mono::subscribe` / `Flux::subscribe`       |
| `Schedulers.immediate()`              | `Scheduler::Immediate`                            |
| `Schedulers.parallel()`               | `Scheduler::Parallel`                             |
| `Schedulers.boundedElastic()`         | `Scheduler::BoundedElastic`                       |
| `subscribeOn` / `publishOn`           | `subscribe_on` / `publish_on`                     |
| `FluxSink` / `Flux.create`            | `FluxSink` / `Flux::create`                       |
| `Retry.backoff(..)`                   | `Backoff` + `*::retry_backoff`                    |
| `Mono.toFuture()` / `await`           | `Mono::into_future` / `.await`                    |
| `Flux.toStream()` (escape hatch)      | `Flux::to_stream` / `Flux::into_stream`           |

## Creating publishers

`Mono` constructors:

| Constructor                       | Produces                                       |
|-----------------------------------|------------------------------------------------|
| `Mono::just(v)`                   | exactly `v`                                    |
| `Mono::just_or_empty(opt)`        | `v` if `Some`, empty if `None`                 |
| `Mono::empty()`                   | completes with no value (`Ok(None)`)           |
| `Mono::error(e)`                  | terminal error                                 |
| `Mono::from_future(fut)`          | awaits a `Future<Output = T>`                  |
| `Mono::from_result_future(fut)`   | awaits a `Future<Output = Result<T, _>>`       |
| `Mono::from_callable(f)`          | runs a `FnOnce() -> Result<Option<T>, _>` on subscribe |
| `Mono::defer(factory)`            | builds the `Mono` fresh per subscription       |

`Flux` constructors:

| Constructor                       | Produces                                       |
|-----------------------------------|------------------------------------------------|
| `Flux::just(vec)`                 | each element of the `Vec`                      |
| `Flux::from_iter(iter)`           | each element of an iterator                    |
| `Flux::range(start, count)`       | `start, start+1, …` (count items)              |
| `Flux::empty()` / `Flux::never()` | completes immediately / never emits            |
| `Flux::error(e)`                  | terminal error                                 |
| `Flux::from_stream(s)`            | a `Stream<Item = Result<T, FireflyError>>`     |
| `Flux::from_value_stream(s)`      | a `Stream<Item = T>`                           |
| `Flux::create(producer)`          | imperative push via a `FluxSink` (see below)   |
| `Flux::interval(period)`          | `0, 1, 2, …` on a timer                         |
| `Flux::generate(seed, step)`      | stateful generation                            |

## Operator quick reference

The operator surface mirrors Reactor. `Mono` and `Flux` share most names; the
differences reflect cardinality.

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

### Transforming and chaining

```rust
use firefly_reactive::{Flux, Mono};

# async fn ex() {
// flat_map: chain a Mono onto the result of another (sequential dependency).
let total = Mono::just(3)
    .flat_map(|seed| Mono::just(seed * 10))
    .map(|x| x + 1)
    .block()
    .await
    .unwrap();
assert_eq!(total, Some(31));

// flat_map on a Flux runs up to N inner publishers concurrently.
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

### Combining

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

## Error semantics

An `Err` item is **terminal** in a `Flux`: every operator short-circuits on the
first error and propagates it downstream — there is no per-element error
channel. To recover:

- `Mono::on_error_return(fallback)` — substitute a value;
- `Mono::on_error_resume(f)` / `Flux::on_error_resume(f)` — switch to a fallback
  publisher, keeping items emitted before the error;
- `Flux::on_error_continue(handler)` — drop the failing element and keep the
  rest (for operators that re-signal per item);
- `Mono::on_error_map(f)` — translate the error.

`retry` and `retry_backoff` re-subscribe to a **factory closure**, since Rust
streams and futures are single-use:

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

`Mono::timeout` / `Flux::timeout` map a missed deadline to a 504 `FireflyError`
(code `REACTIVE_TIMEOUT`), which renders as an RFC 7807 problem response.

## Imperative emission with `FluxSink`

When values arrive from a callback or an imperative loop, push them into a
`Flux` with `Flux::create`:

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

## Schedulers

A `Scheduler` decides *where* work runs. `subscribe_on` hops the source onto a
scheduler; `publish_on` switches the thread for everything downstream.

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

`Scheduler::Immediate` runs inline, `Scheduler::Parallel` uses the Tokio worker
pool for CPU-bound work, and `Scheduler::BoundedElastic` is for blocking calls.

## Reactive HTTP endpoints

`firefly-web` ships responders that turn a `Mono`/`Flux` into an axum response —
the Rust analog of returning `Mono<T>` / `Flux<T>` from a WebFlux
`@RestController`. They reuse `firefly-sse`'s wire format, so every reactive
response is byte-compatible across the ports.

| Spring WebFlux                         | firefly-web                  |
|----------------------------------------|------------------------------|
| `Mono<T>` handler return               | `MonoJson(Mono<T>)`          |
| `Mono<T>` empty → `404`                | `Ok(None)` → 404 problem+json |
| `Mono<T>` error → problem              | `Err(FireflyError)` → that error's RFC 7807 response |
| `Flux<T>` + `APPLICATION_NDJSON_VALUE` | `NdJson(Flux<T>)`            |
| `Flux<ServerSentEvent<T>>`             | `Sse(Flux<T>)` / `SseEvents(Flux<Event>)` |

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

The responders, precisely:

- **`MonoJson(Mono<T>)`** resolves the `Mono`: `Ok(Some)` → `200`
  `application/json`; `Ok(None)` → `404` `application/problem+json`; `Err` →
  that error's problem response.
- **`NdJson(Flux<T>)`** streams `application/x-ndjson` — one compact JSON doc +
  `'\n'` per element, flushed incrementally with real backpressure. The `Flux`'s
  `Stream` is bridged straight into an axum streaming `Body`; the whole stream
  is **never** buffered. An `Err` item mid-stream terminates the body cleanly.
- **`Sse(Flux<T>)`** streams `text/event-stream` — each element serialized to a
  bare `data: <json>\n\n` frame, byte-identical to the `firefly-sse` writer.
- **`SseEvents(Flux<Event>)`** streams pre-built `firefly_sse::Event` values —
  use it when you need `id` / `event` / `retry` fields.

> **Warning** — Backpressure is real, not cosmetic. A slow client throttles the
> producer; nothing is buffered up front. This is what lets a `NdJson` endpoint
> stream a million rows without the response landing fully in memory.

## The reactive `WebClient`

The reactive HTTP client — the Rust analog of WebFlux's `WebClient` — hands its
terminal operators back as `Mono` / `Flux`, so an outbound call drops straight
into a reactive pipeline and composes end-to-end with the `NdJson` / `Sse`
responders above. Full treatment in [HTTP Clients](./13-http-clients.md); the
shape:

```rust,no_run
use firefly_client::WebClientBuilder;
use http::Method;
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

> **Reactor parity** — Like WebFlux, the `WebClient` has **no baked-in retry**.
> Compose `Mono::retry` / `Mono::retry_backoff` on the returned publisher, just
> as WebFlux composes `retryWhen(..)` rather than baking retry into the client.

## Reactive repositories, EDA, and CQRS

The same two types thread through the rest of the framework:

- **Repositories** — `ReactiveCrudRepository<T, ID>` returns `Mono`/`Flux`; the
  Postgres adapter streams rows out of `find_all()` as a `Flux` so a huge table
  never lands fully in memory. See [Persistence](./07-persistence.md).
- **EDA** — `InMemoryBroker::subscribe_reactive(topic)` yields a `Flux<Event>`,
  and `publish_mono(event)` is a cold reactive publish. See [EDA](./10-eda-messaging.md).
- **CQRS** — `Bus::send_mono` / `Bus::query_mono` wrap the dispatch in a lazy
  `Mono<R>`, running the same handler lookup and middleware chain. See
  [CQRS](./09-cqrs.md).

## Interop with raw `Stream` / `Future`

The reactive types are not a walled garden. Convert in and out at the edges:

- **In:** `Flux::from_stream` (a `Stream<Item = Result<T, FireflyError>>`),
  `Flux::from_value_stream` (a `Stream<Item = T>`), `Mono::from_future`,
  `Mono::from_result_future`.
- **Out:** `Flux::to_stream` / `Flux::into_stream`, `Mono::into_future` (or just
  `.await` the `Mono`).

You now have the vocabulary the rest of the book builds on. Next, put it to work
in [Your First HTTP API](./06-first-http-api.md).
