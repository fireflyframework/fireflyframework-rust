# firefly-reactive

A faithful, production-grade **Reactor / WebFlux-style reactive core** for the
Firefly Framework for Rust. It is the Rust analog of [Project Reactor]'s `Mono`
and `Flux` — the engine behind Spring WebFlux and the Java Firefly framework —
and the keystone the reactive Firefly integrations (web, data, client, eda,
cqrs) build on.

- **`Mono<T>`** — a producer of *at most one* value (0-or-1 + error).
- **`Flux<T>`** — a producer of *0..N* values plus a terminal completion/error.
- **`Scheduler`** — where work runs (`Immediate` / `Parallel` / `BoundedElastic`).
- **`FluxSink`** — push values into a `Flux` imperatively (`Flux::create`).
- **`Backoff`** — an exponential retry policy for `retry_backoff`.

The error type is fixed to [`firefly_kernel::FireflyError`], exactly as WebFlux
models everything as a `Throwable`. Fixing the error keeps the operator surface
ergonomic (no error type parameter) and wires straight into the framework's RFC
7807 problem responses. Everything is `Send + 'static`, so a `Mono` or `Flux`
drops directly into an axum handler.

[Project Reactor]: https://projectreactor.io/
[`firefly_kernel::FireflyError`]: https://docs.rs/firefly-kernel

## Quick start

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
    .unwrap()        // Result -> Option
    .unwrap();       // Option -> Vec (collect_list always yields a list)
assert_eq!(xs, vec![10, 30, 50]);
# }
```

## Reactor → firefly-reactive concept map

| Project Reactor                       | firefly-reactive                                |
|---------------------------------------|-------------------------------------------------|
| `Mono<T>`                             | `Mono<T>`                                        |
| `Flux<T>`                             | `Flux<T>`                                        |
| `Throwable` (error signal)            | `firefly_kernel::FireflyError` (fixed)           |
| `Mono.empty()` / `onComplete`         | `Ok(None)` from a `Mono`                         |
| `onError(t)`                          | `Err(FireflyError)` (terminal)                   |
| `Mono.block()`                        | `Mono::block` (async — never parks a thread)     |
| `Mono.subscribe(..)` / `Flux.subscribe(..)` | `Mono::subscribe` / `Flux::subscribe`      |
| `Schedulers.immediate()`              | `Scheduler::Immediate`                           |
| `Schedulers.parallel()`               | `Scheduler::Parallel`                            |
| `Schedulers.boundedElastic()`         | `Scheduler::BoundedElastic`                      |
| `subscribeOn` / `publishOn`           | `subscribe_on` / `publish_on`                    |
| `FluxSink` / `Flux.create`            | `FluxSink` / `Flux::create`                      |
| `Retry.backoff(..)`                   | `Backoff` + `*::retry_backoff`                   |
| `Mono.toFuture()` / `await`           | `Mono::into_future` / `.await`                   |
| `Flux.toStream()` (escape hatch)      | `Flux::to_stream` / `Flux::into_stream`          |

### Operator quick reference

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

## Error semantics

An `Err` item is **terminal** in a `Flux`: every operator short-circuits on the
first error and propagates it downstream — there is no per-element error channel.
To recover:

- `Flux::on_error_resume(f)` — switch to a fallback stream, keeping items emitted
  before the error;
- `Flux::on_error_continue(handler)` — drop the failing element and keep the rest
  (for operators that re-signal per item).

`retry` / `retry_backoff` re-subscribe to a **factory closure**, since Rust
streams and futures are single-use:

```rust
use std::time::Duration;
use firefly_reactive::{Backoff, Mono};
use firefly_kernel::FireflyError;

# async fn ex() {
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
(code `REACTIVE_TIMEOUT`).

## Programmatic emission with `FluxSink`

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

```rust
use firefly_reactive::{Flux, Scheduler};

# async fn ex() {
// Hop the source onto the Tokio worker pool.
let out = Flux::range(1, 3)
    .subscribe_on(Scheduler::Parallel)
    .map(|x| x * 2)
    .collect_list()
    .block()
    .await
    .unwrap();
assert_eq!(out, Some(vec![2, 4, 6]));
# }
```

## Interop with raw `Stream` / `Future`

- In: `Flux::from_stream` (a `Stream<Item = Result<T, FireflyError>>`),
  `Flux::from_value_stream` (a `Stream<Item = T>`), `Mono::from_future`,
  `Mono::from_result_future`.
- Out: `Flux::to_stream` / `Flux::into_stream`, `Mono::into_future` (or just
  `.await` the `Mono`).

## License

Apache-2.0. Part of the [Firefly Framework for Rust](https://github.com/fireflyframework/fireflyframework-rust).
