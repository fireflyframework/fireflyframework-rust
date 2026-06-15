# HTTP Clients

By the end of this chapter, you will know how Lumen reaches *out* — how a wallet
service settles a transfer through an external **Payments** processor, and how an
**experience tier** sits in front of Lumen and composes it with its neighbors
into one journey-shaped API. Lumen itself stays deliberately self-contained
(every leg of its transfer drives the in-process `Ledger`), so this chapter is
the "next adapter you would add": a typed outbound client wired into the transfer
flow, plus the BFF that fans out across services.

When a transfer needs to settle against a real payment rail, the credit leg stops
being a local `Ledger::deposit` and becomes a network call. That call can time
out, fail halfway, or land on an overloaded service — failure modes a local
method call never had. `firefly-client` gives you a typed client for that call
instead of a hand-rolled `reqwest` session threaded with retry and timeout logic.

The crate ships two HTTP clients that share the same automatics — default
`Accept`/`Content-Type`, correlation-id and W3C trace-context propagation, and
RFC 7807 problem decode into a typed `FireflyError`:

- the **eager `RestClient`** (built with `RestBuilder`) — an `async fn` that
  awaits a `Result`, with a built-in retry budget;
- the **reactive `WebClient`** — whose terminal operators hand back `Mono` /
  `Flux`, so an outbound call drops straight into a reactive pipeline.

The crate also ships scaffolds for SOAP, gRPC, GraphQL, and WebSocket clients.
Everything is reachable through the one `firefly` facade as `firefly::client`.

> **Two clients, one contract.** `RestClient` is the eager `async fn`-and-await
> client with a built-in retry budget; `WebClient` is the reactive one whose
> `body_to_mono` / `body_to_flux` / `exchange` terminals return publishers you
> compose. Both are values built with a fluent builder — no annotated interface
> to generate from, no reflection — and the resilience decorators (covered below)
> wrap the call.

## The eager `RestClient`

Build a client with `RestBuilder`, then call `request` with a method, path, and
optional body. The retry budget, timeout, and default headers are configured on
the builder. Here Lumen builds the Payments client it would call from the credit
leg of a transfer:

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
            // Upstream RFC 7807 problems are decoded into a typed FireflyError.
            if let Some(fe) = err.as_firefly() {
                eprintln!("payments upstream {}: {}", fe.status, fe.detail);
            }
        }
    }
}
```

A non-2xx `application/problem+json` response is decoded into a `FireflyError`,
so an upstream failure carries the upstream's status and detail straight through
Lumen's own error stack. `err.as_firefly()` is the typed accessor that recovers
the upstream's typed problem, and `ClientError` also offers
predicate helpers like `is_not_found()`, `is_unprocessable_entity()`, and
`is_retryable()` so a caller can branch on the failure class without matching
raw status codes.

> **Where this plugs into the saga.** In [Sagas](./12-sagas.md) the credit leg
> was `ledger.deposit(&to, amount)`. In a split deployment that becomes
> `payments.request::<_, Payment>(Method::POST, "/settle", …)`. The saga shape
> does not change — it is still a `Step` with the debit's compensation — only
> the *body* of the credit step now does I/O across the network, which is
> exactly why the compensation (refund the debit) matters more than ever.

### Idempotency on the wire

A transfer's settlement call must be idempotent: if `POST /payments` times out
and the saga's retry fires it again, Payments must not create two payments. Carry
a stable `Idempotency-Key` — typically the transfer id — so the upstream
deduplicates a re-delivered request. Set it as a default header on a per-call
builder, or thread it through the request:

```rust,ignore
let payments = RestBuilder::new("https://payments.internal")
    .with_header("Idempotency-Key", &transfer_id) // stable per business op
    .with_timeout(Duration::from_secs(2))
    .build();
```

The deduplication itself is the upstream's job; the client's job is to forward
the key *consistently* across retries.

## The reactive `WebClient`

The reactive client returns `Mono` / `Flux`, so an outbound call drops straight
into a reactive pipeline and composes end-to-end with the
[`NdJson` / `Sse` responders](./05-reactive-model.md) Lumen's streaming endpoint
uses. The fluent chain reads top to bottom — build, address, send, decode:

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
        .body_to_mono::<Payment>();

    // body_to_flux — a streamed NDJSON OR SSE body, decoded lazily
    // element-by-element with backpressure.
    let _ticks = client
        .get()
        .uri("/ledger/ticks")
        .header("Accept", "application/x-ndjson")
        .retrieve()
        .body_to_flux::<LedgerTick>();

    // exchange — raw status + headers without raising on a non-2xx.
    let _resp = client.get().uri("/health").retrieve().exchange();
}
```

The terminal operators:

| Operator                     | Returns                   | Behavior                                       |
|------------------------------|---------------------------|------------------------------------------------|
| `body_to_mono::<T>()`        | `Mono<T>`                 | the whole body decoded as one `T`              |
| `body_to_flux::<T>()`        | `Flux<T>`                 | a streamed NDJSON/SSE body, element-by-element |
| `exchange()`                 | `Mono<WebClientResponse>` | the raw status + headers + body, no raise      |

### Streaming semantics

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
This is the *consumer* side of the
same wire format Lumen's own `GET /api/v1/wallets/:id/events` endpoint produces:
one service streams the wallet's event log, another reads it back element by
element.

### Inspecting the raw response

`exchange()` hands back a `WebClientResponse` **without** raising on a non-2xx,
so you can inspect it and decide:

```rust,ignore
let resp = client.get().uri("/health").retrieve().exchange().block().await?.unwrap();
if resp.is_success() {
    let body: serde_json::Value = resp.body_json()?;
} else if let Some(problem) = resp.problem() {
    // a decoded RFC 7807 FireflyError, if the body was a problem document
}
```

### No baked-in retry

> **Retries are composed, not baked in.** Unlike `RestBuilder::with_retries`,
> the `WebClient` has **no** retry budget. Compose retries on the returned
> publisher with `Mono::retry` / `Mono::retry_backoff`, so retry policy stays a
> property of the call site, not the client:
>
> ```rust,ignore
> use firefly::reactive::{Backoff, Mono};
> use std::time::Duration;
>
> let payment = Mono::retry_backoff(
>     || client.get().uri("/payments/p1").retrieve().body_to_mono::<Payment>(),
>     Backoff::new(3, Duration::from_millis(100)),
> );
> ```

## Composing with resilience

Both clients are deliberately small. For circuit breaking, rate limiting, or
bulkheads, wrap calls in `firefly-resilience` decorators (covered in
[Caching](./17-caching.md) and applied the same way to outbound calls). The
circuit breaker is what keeps a sick Payments service from dragging Lumen down
with it:

```rust,ignore
use firefly::resilience::{CircuitBreaker, CircuitConfig};

// CircuitBreaker::execute returns the operation's value (Result<T, _>), so the
// guarded call still yields the Payment. (Chain::execute is for guarded ops
// whose value you discard — it returns Result<(), _>.)
let breaker = CircuitBreaker::new(CircuitConfig::default());

let payment = breaker.execute(|| async {
    payments.request::<_, Payment>(Method::POST, "/payments", Some(&req)).await
}).await?;
```

When repeated calls fail, the breaker opens and rejects subsequent calls
immediately with `ResilienceError::CircuitOpen` instead of waiting on a timeout —
so one slow upstream cannot exhaust Lumen's task pool. Resilience belongs at the
*client* layer, configured once, not scattered through every handler.

## The experience tier: a Lumen BFF

A mobile or web frontend rarely wants a single domain service's raw shape — it
wants a *journey*: "show me this wallet's balance **and** its pending payments,
in one call." Calling Lumen for the balance and Payments for the pending list and
merging in the client means two round trips, two failure domains, and the
frontend leaking knowledge of both services' internals. The
**Backend-for-Frontend (BFF)** pattern moves that composition server-side.

Firefly ships a dedicated starter for this tier:
`firefly-starter-experience`. It builds on `WebStack` (so it inherits CORS,
security headers, request metrics, correlation, and the actuator surface) and
adds the BFF building blocks:

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
use firefly_starter_experience::{ExperienceStack, DomainClients};
use firefly::starter_web::CoreConfig;

let bff = ExperienceStack::new(CoreConfig {
    app_name: "lumen-mobile-bff".into(),
    ..Default::default()
});

// Register the domain SDKs this BFF composes. `register` returns an
// Arc<RestClient> already wired with correlation + trace propagation.
let wallets = bff.clients.register("wallets", "https://lumen.internal");
let payments = bff.clients.register("payments", "https://payments.internal");
```

The composition root then fans out across the registered clients — both calls
go out concurrently, so the composite latency is bounded by the slower upstream
rather than their sum — and degrades gracefully if one upstream is circuit-open
(show the balance, leave the pending list empty) instead of failing the whole
response.

> **The tier boundary is strict.** The dependency direction is
> `channel → experience → domain → core`. An experience service **never** owns a
> database, **never** calls a core service directly, and **never** calls a
> sibling experience service — it composes *domain* SDKs only. Lumen is a
> domain/core-style service built on the `firefly` facade; the BFF is a separate
> crate that depends on `firefly-starter-experience` and on Lumen's published
> client. That separation is why the experience starter is *not* bundled into
> the one-dependency facade — a domain service does not need it.

> **The BFF pattern.** A BFF is a thin application that aggregates several
> domain services into one journey-shaped API, server-side. `Mono::zip_with`
> (and the concurrent fan-out above) launches the upstream calls together, so the
> composite latency is bounded by the slower upstream rather than their sum, and
> a circuit-open upstream degrades gracefully instead of failing the whole
> response. The team-ownership model follows from the tier boundary: domain teams
> own stable, fine-grained contracts; the frontend team owns the BFF that adapts
> them.

## Other protocols

The crate ships builders/scaffolds for the protocols a back-office platform
needs — SOAP (CXF-style envelope), gRPC, GraphQL, and WebSocket — selected by
feature so heavy dependencies stay out of services that do not use them. The
REST, GraphQL, and SOAP surfaces are fully wired; the streaming protocols (gRPC
and WebSocket) are feature-gated.

Outbound calls inherit the caller's correlation id automatically, so a request
that fans out to three upstreams stitches together in your traces.

## What changed in Lumen

- We sketched the **Payments client** Lumen would build to settle a transfer's
  credit leg over the network — `RestBuilder::new(...).with_retries(...).build()`
  — and showed how an upstream RFC 7807 problem decodes into a typed
  `FireflyError` via `err.as_firefly()`.
- We saw how the saga's credit step changes from a local `Ledger::deposit` to a
  resilient outbound call, why an `Idempotency-Key` is mandatory across retries,
  and why the debit's compensation matters more once I/O can fail.
- We introduced the reactive `WebClient` (`body_to_mono` / `body_to_flux` /
  `exchange`), its streaming decode (the consumer side of Lumen's own NDJSON/SSE
  endpoint), and the rule that retries are *composed* on the returned `Mono`, not
  baked into the client.
- We met the **experience tier** — `firefly-starter-experience` with
  `DomainClients`, `SignalService`, and `WorkflowState` — and saw how a Lumen
  BFF composes the wallet and payments services into one journey-shaped API,
  with the strict `experience → domain → core` dependency direction.

## Exercises

1. **Decode an upstream problem.** Stand up a stub that answers
   `POST /payments` with a `422 application/problem+json` body. Call it through a
   `RestClient` and assert that `err.as_firefly()` returns `Some`, that
   `fe.status == 422`, and that `err.is_unprocessable_entity()` is `true` — so
   the saga can map the upstream rejection onto Lumen's own `422` instead of a
   `500`.

2. **Wrap the credit leg in a circuit breaker.** Take the transfer saga's credit
   step and replace `ledger.deposit(...)` with a `CircuitBreaker`-guarded
   `payments.request(...)`. Drive the stub to fail enough times to trip the
   breaker, and assert the next call returns `ResilienceError::CircuitOpen`
   *immediately* (no timeout) — and that the saga still compensates the debit.

3. **Compose a BFF summary.** Build a tiny `ExperienceStack`, register a
   `wallets` and a `payments` client against two local stubs, and write a handler
   that fetches the balance and the pending list concurrently. Make the payments
   stub return an error and assert the handler still returns the balance with an
   empty pending list — proving partial degradation rather than a `500`.

The next chapter secures the inbound side — JWT bearer auth and path-based RBAC
on Lumen's mutating routes. Continue to [Security](./14-security.md).
