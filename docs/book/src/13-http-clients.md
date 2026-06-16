# HTTP Clients

Until now every leg of a Lumen transfer has been a *local* method call: the
credit step is `ledger.deposit(&req.to, ...)`, in-process, infallible except for
the domain rules it enforces. That is deliberate — Lumen is self-contained, and
keeping it that way let the earlier chapters teach domain modelling, CQRS, event
sourcing, and sagas without a network in the way. This chapter is the moment the
network arrives. It is the *next adapter you would add*: when a transfer has to
settle against a real payment rail, the credit leg stops being a local
`Ledger::deposit` and becomes a call to an external **Payments** service — a call
that can time out, fail halfway, or land on an overloaded host, failure modes a
local method never had.

`firefly-client` gives you a *typed* client for that call instead of a
hand-rolled `reqwest` session threaded with retry and timeout logic. You will
meet three client styles — eager, reactive, and declarative — that all share one
set of automatics, then see how an **experience tier** sits in front of Lumen and
composes it with its neighbours into one journey-shaped API. Everything is
reachable through the one `firefly` facade you have depended on since
[Quickstart](./02-quickstart.md).

By the end of this chapter you will:

- Build an eager `RestClient` with `RestBuilder`, call it, and decode an upstream
  problem document into a typed error.
- Build the reactive `WebClient` and choose between its `body_to_mono` /
  `body_to_flux` / `exchange` terminals — and know why it has *no* baked-in retry.
- Write a declarative `#[http_client]` trait and let the macro generate the
  request-issuing implementation, the mirror image of a `#[rest_controller]`.
- Wrap an outbound call in a `CircuitBreaker` so a sick upstream cannot drag
  Lumen down with it.
- Understand the experience-tier (BFF) pattern and the strict
  `channel → experience → domain → core` dependency direction.

## Concepts you will meet

Before the first client, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — HTTP client.** A *client* here is an object your service
> uses to make *outbound* HTTP calls to another service. It is the inverse of a
> controller, which *receives* inbound calls. Firefly ships a typed client so
> the request shape, the response decode, and the error handling are all checked
> by the compiler instead of left to a raw `reqwest` call.

> **Note** **Key term — RFC 9457 problem document.** A standard JSON error body
> (media type `application/problem+json`) carrying `type`, `title`, `status`,
> and `detail` fields. RFC 9457 is the current standard (it obsoletes RFC 7807).
> Firefly *produces* these from failing handlers and *consumes* them on the
> client side, decoding an upstream problem into a typed `FireflyError` so an
> external failure carries the upstream's status and detail straight through
> Lumen's own error stack.

> **Note** **Key term — correlation id / trace context.** A *correlation id* is a
> per-request identifier that travels with a request so its log lines and the log
> lines of every service it calls can be stitched together. The W3C *trace
> context* (`traceparent` / `tracestate` headers) does the same for distributed
> tracing. Every Firefly client forwards both automatically, so a request that
> fans out to three upstreams stays one coherent trace.

> **Note** **Key term — reactive publisher (`Mono` / `Flux`).** A `Mono<T>` is a
> deferred async value that resolves to at most one `T`; a `Flux<T>` is a
> deferred async *stream* of `T`. You met them in
> [The Reactive Model](./05-reactive-model.md). The reactive client returns them
> so an outbound call drops straight into a reactive pipeline. The Spring analog
> is Project Reactor's `Mono` / `Flux`.

> **Note** **Key term — Backend-for-Frontend (BFF).** A thin server-side
> application that aggregates several domain services into one *journey-shaped*
> API tailored to a particular frontend, instead of making the frontend call
> each service and merge the results itself. Covered in depth in
> [The Experience Tier](./20a-experience-tier.md); introduced here.

The crate ships its clients behind one front door, `firefly::client`, and the
declarative pieces are also re-exported through `firefly::prelude`. The two HTTP
clients share the same automatics — default `Accept` / `Content-Type`,
correlation-id and W3C trace-context propagation, and RFC 9457 problem decode
into a typed `FireflyError`:

- the **eager `RestClient`** (built with `RestBuilder`) — an `async fn` that
  awaits a `Result`, with a built-in retry budget;
- the **reactive `WebClient`** (built with `WebClientBuilder`) — whose terminal
  operators hand back `Mono` / `Flux`, so an outbound call composes end-to-end
  with a reactive pipeline.

On top of the `WebClient` sits the **declarative `#[http_client]`** trait — the
Spring 6 `@HttpExchange` analog — which you write as a trait and let the macro
implement. The crate also ships builders and scaffolds for GraphQL, SOAP, gRPC,
and WebSocket clients, selected by feature so heavy dependencies stay out of
services that do not use them.

> **Design note.** Both HTTP clients are *values built with a fluent builder* —
> there is no annotated interface to generate from for the eager and reactive
> surfaces, and no reflection. The resilience decorators (covered near the end)
> wrap the call from the outside rather than being baked in. That keeps each
> client small and keeps retry/circuit-breaking policy a property of the call
> site, not a hidden default.

## Step 1 — Build the eager `RestClient`

The eager client is the one to reach for when you just want to `await` a result.
You build it with `RestBuilder`, configuring the base URL, default headers, a
per-request timeout, and an attempt budget, then call `request` with a method, a
path, and an optional body.

Here Lumen builds the Payments client it would call from the credit leg of a
transfer:

```rust,no_run
use std::time::Duration;
use firefly::client::RestBuilder;
use http::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct SettleTransfer { wallet_id: String, amount: i64, reference: String }
#[derive(Deserialize)]
struct Payment { id: String, status: String }

#[tokio::main]
async fn main() {
    let payments = RestBuilder::new("https://payments.internal")
        .with_header("X-Tenant", "lumen")
        .with_timeout(Duration::from_secs(5))
        .with_retries(3)
        .build();

    let req = SettleTransfer {
        wallet_id: "wlt_alice".into(),
        amount: 300,
        reference: "transfer-42".into(),
    };
    match payments.request::<_, Payment>(Method::POST, "/payments", Some(&req)).await {
        Ok(payment) => println!("settled {} ({})", payment.id, payment.status),
        Err(err) => {
            // Upstream RFC 9457 problems are decoded into a typed FireflyError.
            if let Some(fe) = err.as_firefly() {
                eprintln!("payments upstream {}: {}", fe.status, fe.detail);
            }
        }
    }
}
```

What just happened, block by block:

- `RestBuilder::new("https://payments.internal")` primes a builder at a base URL
  (trailing slashes are trimmed so `base + path` concatenation stays clean).
- `.with_header("X-Tenant", "lumen")` sets a default header sent on *every*
  request this client makes.
- `.with_timeout(Duration::from_secs(5))` caps each attempt at five seconds.
- `.with_retries(3)` sets the *total attempt budget* to three. Note this is the
  number of attempts, not extra retries: `1` means one attempt with no retry, and
  the client retries only on network errors and `429` / `5xx` statuses, with
  exponential backoff (100 ms doubling per attempt, capped at 2 s).
- `.build()` finalises the `RestClient`.
- `payments.request::<_, Payment>(Method::POST, "/payments", Some(&req))` sends
  the request: the turbofish names the body type (inferred here) and the response
  type `Payment`. It JSON-encodes the body, sets `Content-Type` /
  `Accept: application/json`, forwards the correlation id and trace context, and
  decodes a 2xx body into `Payment`.

Why it matters: a non-2xx `application/problem+json` response is decoded into a
`FireflyError`, so an upstream failure carries the upstream's status and detail
straight through Lumen's own error stack. `err.as_firefly()` is the typed
accessor that recovers the upstream's decoded problem.

> **Tip** **Checkpoint.** You can call `request::<_, T>(method, path, body)` and
> get back a `Result<T, ClientError>`. On the error path, `err.as_firefly()`
> returns `Some(&FireflyError)` whenever the failure was an upstream HTTP error
> (not a transport / encode / decode failure), and `fe.status` / `fe.detail`
> echo the upstream's problem.

### Branching on the failure class

You rarely want to match raw status codes. `ClientError` offers predicate helpers
so a caller can branch on the *class* of failure:

```rust,ignore
match payments.request::<_, Payment>(Method::POST, "/payments", Some(&req)).await {
    Ok(payment) => { /* settled */ }
    Err(err) if err.is_unprocessable_entity() => { /* 422 — map onto Lumen's own 422 */ }
    Err(err) if err.is_retryable()            => { /* 429 / 5xx / transport — worth a retry */ }
    Err(err) => { /* everything else */ }
}
```

The predicates mirror the way the framework renders problems elsewhere:
`is_validation()` (400), `is_unauthorized()` (401/403), `is_not_found()` (404),
`is_conflict()` (409), `is_unprocessable_entity()` (422),
`is_rate_limited()` (429), `is_server_error()` (5xx), and `is_retryable()` (the
same rule the client applies internally — transport failures, `429`, and any
`5xx`).

> **Note** **Where this plugs into the saga.** In [Sagas](./12-sagas.md) the
> credit leg was `ledger.deposit(&req.to, amount)`. In a split deployment that
> becomes `payments.request::<_, Payment>(Method::POST, "/payments", …)`. The
> saga *shape* does not change — it is still a `#[saga_step]` with the debit's
> `compensate = "refund_debit"` — only the *body* of the credit step now does I/O
> across the network, which is exactly why the compensation (refund the debit)
> matters more than ever.

## Step 2 — Keep the settlement call idempotent

A transfer's settlement call must be idempotent: if `POST /payments` times out
and the saga's retry fires it again, Payments must not create *two* payments.
Carry a stable `Idempotency-Key` — typically the transfer id — so the upstream
deduplicates a re-delivered request. Set it as a default header on a per-call
builder:

```rust,ignore
let payments = RestBuilder::new("https://payments.internal")
    .with_header("Idempotency-Key", &transfer_id) // stable per business op
    .with_timeout(Duration::from_secs(2))
    .build();
```

What just happened: because the key is a default header, *every* attempt this
client makes — including the retries the budget triggers — carries the same key.
The deduplication itself is the upstream's job; the client's job is to forward
the key *consistently* across retries.

Why it matters: this is the outbound mirror of the inbound idempotency you got
for free in [Quickstart](./02-quickstart.md) — there Lumen *records* an
`Idempotency-Key` and replays the stored response; here Lumen *sends* one so the
service it calls can do the same.

> **Tip** **Checkpoint.** A retried `POST` carrying a stable `Idempotency-Key`
> reaches the upstream with the *same* key every time. If you set the key per
> attempt instead of per business operation, deduplication breaks — make it a
> default header keyed on the business id (the transfer id), not on the attempt.

## Step 3 — Build the reactive `WebClient`

The reactive client returns `Mono` / `Flux`, so an outbound call drops straight
into a reactive pipeline and composes end-to-end with the `NdJson` / `Sse`
responders ([The Reactive Model](./05-reactive-model.md)) Lumen's streaming
endpoint uses. You build it with `WebClientBuilder`; the fluent request chain
reads top to bottom — build, address, send, decode:

```rust,no_run
use firefly::client::WebClientBuilder;
use http::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct SettleTransfer { wallet_id: String, amount: i64 }
#[derive(Deserialize)]
struct Payment { id: String }
#[derive(Deserialize)]
struct LedgerTick { seq: u64 }

#[tokio::main]
async fn main() {
    let client = WebClientBuilder::new("https://payments.internal")
        .with_header("X-Tenant", "lumen")
        .build();

    // body_to_mono — a single value -> Mono<Payment>.
    let _payment = client
        .method(Method::POST)
        .uri("/payments")
        .body(&SettleTransfer { wallet_id: "wlt_alice".into(), amount: 300 })
        .retrieve()
        .body_to_mono::<Payment>()
        .block()
        .await;

    // body_to_flux — a streamed NDJSON OR SSE body, decoded lazily
    // element-by-element with backpressure.
    let _ticks = client
        .get()
        .uri("/ledger/ticks")
        .header("Accept", "application/x-ndjson")
        .retrieve()
        .body_to_flux::<LedgerTick>()
        .collect_list()
        .block()
        .await;

    // exchange — raw status + headers without raising on a non-2xx.
    let _resp = client.get().uri("/health").retrieve().exchange().block().await;
}
```

What just happened, reading one chain at a time:

- `client.method(Method::POST)` (or the `.get()` / `.post()` / `.put()` /
  `.delete()` / `.patch()` shorthands) starts a request; `.uri(...)` sets the
  path; `.body(&...)` JSON-encodes a body; `.retrieve()` finalises the request
  into a *response spec*. No I/O has happened yet — the request is sent lazily
  when the returned publisher is subscribed.
- `.body_to_mono::<Payment>()` says "decode the whole body as one `Payment`" and
  yields a `Mono<Payment>`. `.block().await` subscribes and waits, returning
  `Result<Option<Payment>, FireflyError>` — the `Result` carries any terminal
  error, the `Option` models an empty (`204`) body.
- `.body_to_flux::<LedgerTick>()` says "decode this streamed body
  element-by-element" and yields a `Flux<LedgerTick>`; `.collect_list()` gathers
  it into a `Mono<Vec<LedgerTick>>`.
- `.exchange()` hands back the raw response (status + headers + body) *without*
  raising on a non-2xx, as a `Mono<WebClientResponse>`.

> **Note** **Key term — terminal operator.** A *terminal operator* is the method
> that ends the fluent chain and decides the shape of the result. On a
> `WebClient`'s response spec the three terminals are:
>
> | Operator                | Returns                   | Behavior                                       |
> |-------------------------|---------------------------|------------------------------------------------|
> | `body_to_mono::<T>()`   | `Mono<T>`                 | the whole body decoded as one `T`              |
> | `body_to_flux::<T>()`   | `Flux<T>`                 | a streamed NDJSON/SSE body, element-by-element |
> | `exchange()`            | `Mono<WebClientResponse>` | the raw status + headers + body, no raise      |

> **Tip** **Checkpoint.** A `WebClient` chain that ends in `.body_to_mono::<T>()`
> gives you a `Mono<T>` you can `.block().await` (yielding
> `Result<Option<T>, FireflyError>`) or compose further. Nothing fires until you
> subscribe — if you build the chain and never block/await it, no request is sent.

## Step 4 — Stream a response with `body_to_flux`

`body_to_flux` consumes the byte stream chunk-by-chunk and decodes one element
per frame, lazily and with backpressure — a slow downstream throttles the
producer, and `.take(n)` stops pulling early. The decoder is chosen from the
response `Content-Type`:

- `application/x-ndjson` (and any non-SSE type) → one JSON document per
  newline-terminated line;
- `text/event-stream` → SSE frames separated by a blank line; the `data:` lines
  are concatenated and comment / `event:` / `id:` lines are ignored.

A malformed element terminates the stream with a decode `FireflyError` — the
first error is terminal, the reactive-streams contract Firefly's `Flux` honors.

Why it matters: this is the *consumer* side of the same wire format Lumen's own
`GET /api/v1/wallets/:id/events` endpoint (the feature-gated `streaming`
endpoint) *produces*. One service streams the wallet's event log; another reads
it back element by element — the exact symmetry the reactive model buys you.

> **Tip** **Checkpoint.** Point a `body_to_flux::<T>()` at an
> `application/x-ndjson` endpoint and `.take(5)` it; only five elements are
> pulled and the upstream stops producing. Point it at a `text/event-stream`
> endpoint and the SSE `data:` frames decode the same way — the content type, not
> the call, picks the decoder.

## Step 5 — Inspect the raw response with `exchange`

`exchange()` hands back a `WebClientResponse` *without* raising on a non-2xx, so
you can inspect the status and decide what to do — the right terminal when a
non-2xx is *expected* and should not short-circuit the pipeline:

```rust,ignore
let resp = client.get().uri("/health").retrieve().exchange().block().await?.unwrap();
if resp.is_success() {
    let body: serde_json::Value = resp.body_json()?;
} else if let Some(problem) = resp.problem() {
    // a decoded RFC 9457 FireflyError, if the body was a problem document
}
```

What just happened: `.exchange().block().await` returns
`Result<Option<WebClientResponse>, FireflyError>`; the `?` unwraps the `Result`
(only a transport-level failure errors here) and `.unwrap()` the `Option`.
`resp.is_success()` tests the 2xx range, `resp.body_json::<T>()` decodes the
buffered body, and `resp.problem()` decodes a non-2xx `application/problem+json`
body into a `FireflyError` (returning `None` for a 2xx). The difference from
`body_to_mono` is the *raise* behaviour: `body_to_mono` turns a non-2xx into the
`Mono`'s terminal `Err`, while `exchange` hands you the raw response to branch on.

## Step 6 — Compose retries (the `WebClient` bakes in none)

Unlike `RestBuilder::with_retries`, the `WebClient` has **no** retry budget. That
is intentional: retry policy stays a property of the *call site*, not the client.
Compose retries on the returned publisher with `Mono::retry` /
`Mono::retry_backoff`:

```rust,ignore
use firefly::reactive::{Backoff, Mono};
use std::time::Duration;

let payment = Mono::retry_backoff(
    || client.get().uri("/payments/p1").retrieve().body_to_mono::<Payment>(),
    Backoff::new(3, Duration::from_millis(100)),
);
```

What just happened: `Mono::retry_backoff` takes a *factory closure* (it must
rebuild the request on each attempt, since a subscribed `Mono` is consumed) and a
`Backoff::new(max_retries, base)` schedule. Each failure re-runs the factory after
an exponentially growing delay. `Mono::retry(factory, n)` is the fixed-count
sibling with no backoff.

Why it matters: the same `WebClient` can be cautious on one endpoint and
aggressive on another, because the policy lives on the call rather than the
client. This mirrors how the reactive model composes `retry` onto a publisher
rather than configuring it once globally.

> **Tip** **Checkpoint.** A `WebClient` call wrapped in `Mono::retry_backoff`
> retries on its own schedule; the bare `WebClient` never retries. If you find
> yourself wishing `WebClientBuilder` had a `.with_retries`, that is the signal to
> reach for `Mono::retry_backoff` instead.

## Step 7 — Write a declarative `#[http_client]` trait

Writing the call chain by hand is fine for one-off requests, but a *service you
call repeatedly* deserves a typed interface. `#[http_client]` is the analog of
Spring 6's `@HttpExchange` (the modern OpenFeign replacement): you write a
**trait** of methods carrying the same verb attributes a `#[rest_controller]`
uses, and the macro generates a `<Trait>Impl` that issues the requests over a
`WebClient`. It is the mirror image of a controller — same vocabulary, request
*issued* instead of *received*.

> **Note** **Key term — declarative client.** A *declarative client* is an
> interface you *describe* (verbs, paths, arguments) and let the framework
> *implement*, instead of writing the request-issuing code yourself. The macro
> reads the trait and generates the body. The Spring analog is `@HttpExchange` on
> a Java interface (formerly Spring Cloud OpenFeign's `@FeignClient`).

```rust,ignore
use firefly::prelude::*;            // #[http_client], ClientError, Mono, Flux
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct CreateOrder { pub sku: String, pub qty: u32 }
#[derive(Deserialize)]
pub struct Order { pub id: String, pub sku: String }

#[http_client(path = "/api/v1/orders", name = "orders", bean)]
pub trait OrdersClient {
    // `:id` name-matches the `id` arg → path variable (percent-encoded).
    #[get("/:id")]
    async fn get_order(&self, id: String) -> Result<Order, ClientError>;

    // `status` / `page` are neither path vars nor a body → inferred query
    // params; `Option` omits itself when `None`.
    #[get("/")]
    async fn list(&self, status: String, page: Option<u32>) -> Result<Vec<Order>, ClientError>;

    // the lone non-scalar arg is the JSON body; one explicit header.
    #[post("/")]
    async fn create(&self, #[header("X-Tenant")] tenant: String, order: CreateOrder)
        -> Result<Order, ClientError>;

    #[delete("/:id")]
    async fn cancel(&self, id: String) -> Result<(), ClientError>;   // 204 → ()

    // reactive-first: a non-async fn returning Mono/Flux, no bridging.
    #[get("/stream")]
    fn stream(&self) -> Flux<Order>;
}
```

What just happened: the macro emitted the trait (minus the verb / per-arg marker
attributes) plus a concrete `OrdersClientImpl` struct that wraps a `WebClient` and
implements the trait by translating each method's verb, path template, and bound
arguments into a fluent `WebClient` request. The trait-level
`path = "/api/v1/orders"` is joined onto every method path; `name = "orders"`
names the DI bean; `bean` opts into registration (Step 8).

Construct it from a base URL, or inject a tuned `WebClient`:

```rust,ignore
let api = OrdersClientImpl::new("https://orders.svc");      // builds a WebClient
let order = api.get_order("42".into()).await?;
// or: OrdersClientImpl::with_client(my_web_client)         // shared pool / timeouts
```

`OrdersClientImpl::new(base_url)` builds a fresh `WebClient` rooted at the URL (and
applies the trait's `accept` / `content_type` defaults, if any).
`OrdersClientImpl::with_client(web_client)` is the DI seam — pass an
already-configured `WebClient` (timeouts, default headers, a shared connection
pool), the analog of Spring's `HttpServiceProxyFactory`.

### How arguments bind

Path syntax is the framework's `:id` (the same as `#[rest_controller]`), not
Spring's `{id}` — so a controller and its mirror-image client read identically,
and writing `{id}` is a compile error pointing you at `:id`. Argument binding
needs no attributes in the common case:

- an unannotated argument whose name matches a `:var` segment is the **path
  variable** (percent-encoded);
- the lone unannotated non-scalar argument on a `POST` / `PUT` / `PATCH` is the
  **JSON body**;
- everything else is a **query param** (`Option` omits itself when `None`;
  `Vec` / `&[_]` repeats the key).

Override any of these with `#[path]` / `#[query("k")]` / `#[header("X")]` /
`#[body]`. Every `:var` must bind to exactly one argument or the macro refuses to
compile, so a rename surfaces loudly instead of silently dropping the value.

### Return shapes

An `async fn` returning `Result<T, ClientError>` is the ergonomic default;
`Result<T, E>` works for any `E: From<ClientError>`; a *non-async* `fn` returning
`Mono<T>` / `Flux<T>` hands back the deferred reactive value directly (a `Flux`
defaults to `Accept: application/x-ndjson`); and `WebClientResponse` is the raw
`.exchange()` escape hatch.

> **Note** **Error fidelity.** On an awaited `Result<T, ClientError>` method every
> failure arrives as `ClientError::Problem` carrying a `FireflyError` with the
> original status and code — so `is_not_found()` / `is_server_error()` /
> `is_retryable()` still classify correctly — rather than the structured
> `Transport` / `Decode` / `Encode` variants. Those structured variants survive
> only on the `Mono` / `Flux` return forms (where the `FireflyError` terminal *is*
> the reactive error channel). Match on the reactive form when you need byte-exact
> variants.

> **Tip** **Checkpoint.** A trait under `#[http_client]` produces a
> `<Trait>Impl` you can construct with `::new(url)`. Calling `get_order("42".into())`
> issues `GET /api/v1/orders/42` and decodes the body into `Order`. If you typo a
> `:var` so it binds no argument, the build fails — that is the macro doing its
> job.

## Step 8 — Autowire the client as a bean

With `#[http_client(... bean)]` the generated `OrdersClientImpl` is registered as
a `@Service`-style bean and bound to `dyn OrdersClient`, so a collaborator just
declares `#[autowired] orders: Arc<dyn OrdersClient>` and the container resolves
it — the Feign-client autowire payoff you met in
[Dependency Wiring](./04-dependency-wiring.md). Registration pulls a shared
`WebClient` bean from the container (a named one when you write `client = "…"`),
so every declarative client over the same upstream can share one tuned connection
pool.

What just happened: `bean` ties the declarative client into the same DI graph
that wires Lumen's controllers and handlers. The trait must be object-safe for the
`dyn` bind (the macro checks this up front and adds the `Send + Sync`
supertraits), so a non-object-safe shape fails with a clear message instead of a
downstream `dyn Trait` error.

> **Tip** **Checkpoint.** A `#[http_client(... bean)]` trait makes
> `Arc<dyn OrdersClient>` an injectable dependency. Add `#[autowired] orders:
> Arc<dyn OrdersClient>` to any bean and the container hands you the generated
> impl — no manual construction at the call site.

## Step 9 — Wrap the call in a circuit breaker

Both clients are deliberately small. For circuit breaking, rate limiting, or
bulkheads, wrap calls in `firefly-resilience` decorators (the same ones
[Caching](./17-caching.md) applies to inbound work, applied the same way to
outbound calls). The circuit breaker is what keeps a sick Payments service from
dragging Lumen down with it.

> **Note** **Key term — circuit breaker.** A *circuit breaker* watches a
> dependency's recent failures. After enough failures it *opens* and rejects
> further calls immediately for a cooldown, instead of letting every caller wait
> on a doomed timeout — then it half-opens to probe recovery. The Spring/Java
> analog is Resilience4j's `CircuitBreaker`.

```rust,ignore
use firefly::resilience::{CircuitBreaker, CircuitConfig};

// CircuitBreaker::execute returns the operation's value (Result<T, _>), so the
// guarded call still yields the Payment.
let breaker = CircuitBreaker::new(CircuitConfig::default());

let payment = breaker.execute(|| async {
    payments.request::<_, Payment>(Method::POST, "/payments", Some(&req)).await
}).await?;
```

What just happened: `CircuitBreaker::new(CircuitConfig::default())` builds a
breaker; `breaker.execute(|| async { ... })` runs the closure under supervision,
recording each outcome and propagating the operation's `Result<T, _>` (so the
guarded call still yields the `Payment`).

Why it matters: when repeated calls fail, the breaker opens and rejects
subsequent calls immediately with `ResilienceError::CircuitOpen` instead of
waiting on a timeout — so one slow upstream cannot exhaust Lumen's task pool.
Resilience belongs at the *client* layer, configured once, not scattered through
every handler.

> **Tip** **Checkpoint.** Drive the upstream to fail enough times and the next
> `breaker.execute(...)` returns `Err(ResilienceError::CircuitOpen)` *immediately*
> — no timeout wait. `err.is_circuit_open()` confirms it.

## Step 10 — Meet the experience tier (a Lumen BFF)

A mobile or web frontend rarely wants a single domain service's raw shape — it
wants a *journey*: "show me this wallet's balance **and** its pending payments, in
one call." Calling Lumen for the balance and Payments for the pending list and
merging in the client means two round trips, two failure domains, and the frontend
leaking knowledge of both services' internals. The Backend-for-Frontend (BFF)
pattern moves that composition server-side.

Firefly ships a dedicated starter for this tier, `firefly-starter-experience`. It
builds on the same `WebStack` Lumen uses (so it inherits CORS, security headers,
request metrics, correlation, and the actuator surface) and adds the BFF building
blocks:

- `DomainClients` — a registry of named `RestClient`s for the downstream domain
  services;
- `SignalService` — gates a long-running, signal-driven workflow step parks on
  until a caller delivers a named signal (the experience-tier `Workflow` from
  [Sagas](./12-sagas.md));
- a Redis-capable `WorkflowState` keyed by correlation id, plus a
  `WorkflowQueryService` for journey-status reads.

A Lumen experience service registers its downstream clients up front, then
composes them:

```rust,ignore
use firefly::starter_experience::{ExperienceStack, CoreConfig};

let bff = ExperienceStack::new(CoreConfig {
    app_name: "lumen-mobile-bff".into(),
    ..Default::default()
});

// Register the domain SDKs this BFF composes. `register` returns an
// Arc<RestClient> already wired with correlation + trace propagation.
let wallets = bff.clients.register("wallets", "https://lumen.internal");
let payments = bff.clients.register("payments", "https://payments.internal");
```

What just happened: `ExperienceStack::new(CoreConfig { app_name, .. })` wires the
web tier plus the BFF building blocks; `bff.clients` is the `DomainClients`
registry, and `register(name, base_url)` returns an `Arc<RestClient>` already
wired with correlation + trace propagation. The composition root then fans out
across the registered clients — both calls go out concurrently, so the composite
latency is bounded by the slower upstream rather than their sum — and degrades
gracefully if one upstream is circuit-open (show the balance, leave the pending
list empty) instead of failing the whole response.

> **Note** The experience tier has a chapter of its own —
> [The Experience Tier](./20a-experience-tier.md) — which covers
> `SignalService`, `WorkflowState`, the concurrent fan-out (`Mono::zip_with`),
> and partial-degradation handlers in depth. This section is the introduction;
> the full treatment lives there.

> **Design note.** The tier boundary is strict: the dependency direction is
> `channel → experience → domain → core`. An experience service *never* owns a
> database, *never* calls a core service directly, and *never* calls a sibling
> experience service — it composes *domain* SDKs only. Lumen is a domain/core-style
> service built on the `firefly` facade; the BFF is a separate crate that depends
> on `firefly-starter-experience` and on Lumen's published client. That separation
> is why the experience starter is *not* bundled into the one-dependency facade —
> a domain service does not need it.

## Other protocols

Beyond REST, the crate ships builders and scaffolds for the protocols a
back-office platform needs, selected by feature so heavy dependencies stay out of
services that do not use them:

- `GraphQlBuilder` / `GraphQlClient` — POST a `{ query, variables?,
  operationName? }`, raise `ClientError::GraphQl` on a non-empty `errors` array,
  decode `data` into a typed `T`. Always available (no extra deps).
- `SoapBuilder` / `SoapClient` — wrap a body in a SOAP 1.1 envelope, POST
  `text/xml` with an optional `SOAPAction` header, return the raw response XML.
  Always available.
- `GrpcBuilder` — build a `tonic` channel for a caller-supplied generated stub.
  Behind the `grpc` feature (`grpc-tls` for TLS).
- `WsBuilder` / `WsClient` — connect and stream over `tokio-tungstenite`. Behind
  the `websocket` feature.

The REST, GraphQL, and SOAP surfaces are fully wired; the streaming protocols
(gRPC and WebSocket) are feature-gated. As with the HTTP clients, every outbound
call inherits the caller's correlation id automatically, so a request that fans
out to three upstreams stitches together in your traces.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| every transfer leg is a local `ledger.deposit(...)` | the credit leg can become a resilient outbound `payments.request(...)` over the network |
| no outbound failure modes | upstream RFC 9457 problems decode into a typed `FireflyError`, classified by `is_*` predicates |
| no network idempotency | a stable `Idempotency-Key` forwarded consistently across retries |
| — | the reactive `WebClient` (`body_to_mono` / `body_to_flux` / `exchange`), with retries *composed* via `Mono::retry_backoff`, not baked in |
| — | declarative `#[http_client]` traits autowired as `Arc<dyn …>` beans, plus a `CircuitBreaker`-guarded call |

You also now know:

- That the eager `RestClient`, the reactive `WebClient`, and the declarative
  `#[http_client]` share one set of automatics — default headers, correlation /
  trace propagation, and RFC 9457 problem decode.
- Why the `WebClient` has no retry budget: retry policy is a property of the call
  site, expressed with `Mono::retry` / `Mono::retry_backoff`.
- That a declarative client mirrors a `#[rest_controller]` (same `:id` path
  syntax, same verb attributes), and `bean` ties it into the DI graph.
- The experience-tier (BFF) pattern and its strict `channel → experience →
  domain → core` boundary — the full treatment of which lives in
  [The Experience Tier](./20a-experience-tier.md).

## Exercises

1. **Decode an upstream problem.** Stand up a stub that answers `POST /payments`
   with a `422 application/problem+json` body. Call it through a `RestClient` and
   assert that `err.as_firefly()` returns `Some`, that `fe.status == 422`, and
   that `err.is_unprocessable_entity()` is `true` — so the saga can map the
   upstream rejection onto Lumen's own `422` instead of a `500`.

2. **Compose a retry.** Wrap a `WebClient` call to a flaky stub in
   `Mono::retry_backoff(|| …, Backoff::new(3, Duration::from_millis(50)))`. Make
   the stub fail twice then succeed, and assert the call ultimately resolves —
   then confirm the bare `WebClient` (no wrapper) gives up after one attempt.

3. **Wrap the credit leg in a circuit breaker.** Take the transfer saga's credit
   step and replace `ledger.deposit(...)` with a `CircuitBreaker`-guarded
   `payments.request(...)`. Drive the stub to fail enough times to trip the
   breaker, and assert the next call returns `ResilienceError::CircuitOpen`
   *immediately* (no timeout) — and that the saga still compensates the debit.

4. **Write a declarative client.** Define a `#[http_client(path = "/api/v1/orders")]`
   trait with a `get_order(&self, id: String) -> Result<Order, ClientError>`
   method, construct it with `::new("http://localhost:PORT")` against a local
   stub, and assert it issues `GET /api/v1/orders/42`. Then change the path to use
   Spring's `{id}` and confirm the build fails with the `:id` hint.

5. **Compose a BFF summary.** Build a tiny `ExperienceStack`, register a `wallets`
   and a `payments` client against two local stubs, and write a handler that
   fetches the balance and the pending list concurrently. Make the payments stub
   return an error and assert the handler still returns the balance with an empty
   pending list — proving partial degradation rather than a `500`.

## Where to go next

- Secure the *inbound* side — JWT bearer auth and path-based RBAC on Lumen's
  mutating routes — in **[Security](./14-security.md)**.
- Go deeper on composing domain services into a journey-shaped API in
  **[The Experience Tier](./20a-experience-tier.md)**.
- Revisit the resilience decorators (circuit breaker, rate limiter, bulkhead,
  timeout) applied to inbound work in **[Caching](./17-caching.md)**.
