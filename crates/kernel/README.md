# `firefly-kernel`

> **Tier:** Foundational · **Status:** Full · **Java original:** `firefly-common` · **Go module:** `kernel`

## Overview

`firefly-kernel` is the **shared-vocabulary tier** of the framework. It
exposes the four primitives every Firefly crate agrees on:

1. The RFC 7807 [`ProblemDetail`](#problemdetail-rfc-7807) envelope.
2. The [`FireflyResult<T>`](#fireflyresultt) success-or-failure alias.
3. The [`Clock`](#clock) abstraction for testable time.
4. The [`FireflyError`](#fireflyerror--helpers) typed error family.

Every method in every other crate returns one of these types. The
wire shape is identical to the Java firefly-common module, the .NET
`FireflyFramework.Kernel` project, the Go `kernel` module, and the
Python `pyfly` kernel — a service running version `X` on any of the
runtimes emits the same JSON.

## Why a separate crate?

Java's `Throwable` hierarchy and .NET's `Exception` family are stable
language fixtures. Rust's `std::error::Error` trait is intentionally
minimal — which means every framework that wants typed error codes /
structured fields / HTTP status mapping has to invent its own.
`firefly-kernel` provides the canonical type so the whole platform
agrees, and so the wire is identical across runtimes.

## Public surface

### `ProblemDetail` (RFC 7807)

The canonical `application/problem+json` envelope.

| Member         | Behaviour                                                      |
|----------------|----------------------------------------------------------------|
| `problem_type` | URI reference identifying the problem class (JSON `type`)      |
| `title`        | Short, human-readable summary                                  |
| `status`       | HTTP status code                                               |
| `detail`       | Specific to this occurrence                                    |
| `instance`     | URI of the request that produced the problem                   |
| `extensions`   | RFC 7807 §3.2 extension members; flattened on `Serialize`      |

Empty standard members are omitted on the wire and standard members win
on key collision with extensions, exactly as in the Go port. Serialized
bytes match Go's `json.Marshal` exactly: keys are lexicographically
ordered and strings carry Go's default HTML escaping — `<`, `>`, `&`
and U+2028/U+2029 are written as the u003c, u003e, u0026, u2028 and
u2029 Unicode escapes.
Constructors emit the canonical type URIs
(`https://fireflyframework.org/problems/<kind>`):
`ProblemDetail::bad_request`, `unauthorized`, `forbidden`, `not_found`,
`conflict`, `unprocessable`, `rate_limited`, `internal`, `validation`.

### `FireflyResult<T>`

Where the Go port carries a generic `Result[T]` envelope (Go has no
native result type), Rust already has one:

```rust,ignore
pub type FireflyResult<T> = Result<T, FireflyError>;
```

`map`, `and_then`, and the `?` operator replace Go's `MapResult`,
`FlatMapResult`, and `Value()` helpers.

### `Clock`

```rust,ignore
pub trait Clock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}
```

Implementations: `SystemClock`, `FixedClock(t)`, `MutableClock`
(thread-safe; `advance(d)` for tests; `Default` is the Unix epoch).

### `FireflyError` + helpers

```rust,ignore
pub struct FireflyError {
    pub code: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub fields: BTreeMap<String, serde_json::Value>,
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}
```

Constructors `FireflyError::bad_request(...)`, `unauthorized(...)`,
`forbidden(...)`, `not_found(...)`, `conflict(...)`, `validation(...)`,
`rate_limited(...)`, `internal(...)`, `idempotency_conflict(...)` return
values whose `source()` chain behaves like Go's `errors.Is` /
`errors.As`. Helpers: `is_firefly(&err)`, `status_of(&err)`,
`as_problem(&err)` (renders any `std::error::Error` as a
`ProblemDetail`) — each walks the full source chain.

### Correlation context

```rust,ignore
let out = with_correlation_id("abc-123", async {
    correlation_id() // Some("abc-123")
}).await;
let fresh = new_correlation_id(); // 32-char hex
```

Go's `context.Context` value becomes a tokio task-local scope; nested
scopes shadow like child contexts. `HEADER_CORRELATION_ID`
(`X-Correlation-Id`) and `HEADER_IDEMPOTENCY_KEY` (`Idempotency-Key`)
are exported for cross-crate agreement.

### Version

`firefly_kernel::VERSION` is the released framework version
(`"26.6.1"` at the time of writing — the Go port's CalVer `26.05.01`
expressed as valid semver) — embedded in the actuator `/version`
payload and the startup banner.

## pyfly parity

The pyfly-parity layer adds three surfaces, all additive.

### `ddd` module — the `pyfly.domain` kit

`firefly_kernel::ddd` (every item also re-exported at the crate root)
is the zero-dependency DDD building-block kit:

| Item                            | pyfly original                                       |
|---------------------------------|------------------------------------------------------|
| `Specification<T>`              | `Specification[T]` (`is_satisfied_by`)               |
| `.and()` / `.or()` / `.not()`   | the `&` / `\|` / `~` combinators (`AndSpec`/`OrSpec`/`NotSpec`) |
| blanket impl for `Fn(&T) -> bool` | `Specification.of(callable)`                       |
| `Entity` trait                  | `Entity[TID]` — `id()`, `is_transient()`, `same_identity()` |
| `PendingEvents<E>`              | `AggregateRoot` — `raise` / `pending` / `drain`      |
| `EventMeta`                     | `DomainEvent.event_id` (UUID v4) + `occurred_at` (UTC) |
| `TransientDomainEvent`          | `DomainEvent` — `event_type()` defaults to the short type name |
| `BoxedDomainEvent`              | the untyped `list[DomainEvent]` heterogeneous buffer |

```rust,ignore
use firefly_kernel::ddd::{PendingEvents, Specification};

struct IsAdult;
impl Specification<Customer> for IsAdult {
    fn is_satisfied_by(&self, c: &Customer) -> bool { c.age >= 18 }
}
let premium_adult = IsAdult.and(|c: &Customer| c.premium);
```

This is the **non-event-sourced** aggregate primitive — state persists
through repositories and events are merely collected for post-commit
publication. The event-sourced variant (versioned, wire-formatted,
`EventStore`-coupled) lives in `firefly-eventsourcing`. pyfly's
`ValueObject` maps to native `Clone` + struct-update syntax;
`DomainRepository` maps to `firefly_data::Repository<T, K>` (`save` ~
`add`, `find_by_id` ~ `find`, `delete` ~ `remove`, `next_id` ~
`Uuid::new_v4()`).

### Domain error constructors

`FireflyError::business_rule(rule, detail)` — pyfly's
`BusinessRuleViolation`: code `DOMAIN_RULE_VIOLATION`, status 422, the
rule name in the `rule` field, defaulting the detail to
`Business rule violated: <rule>`. `FireflyError::aggregate_not_found(
aggregate_type, id)` — pyfly's `AggregateNotFound`: code
`DOMAIN_AGGREGATE_NOT_FOUND`, status 404, structured `aggregate_type` /
`id` fields.

### Request-id and tenant-id scopes

Two new task-local scopes mirror the correlation id, porting pyfly's
`pyfly.observability.correlation` context vars:

```rust,ignore
let out = with_request_id("req-1", async {
    with_tenant_id("acme", async {
        (request_id(), tenant_id()) // (Some("req-1"), Some("acme"))
    }).await
}).await;
let fresh = new_request_id(); // 32-char hex, generated per HTTP call
```

`with_request_id_sync` / `with_tenant_id_sync` cover blocking code.
Header constants: `HEADER_REQUEST_ID` (`X-Request-Id`, generated when
absent) and `HEADER_TENANT_ID` (`X-Tenant-Id`, never generated
server-side — `None` means "unscoped").

## Quick start

```rust
use firefly_kernel::{FireflyError, FireflyResult};

fn charge(order_id: &str) -> FireflyResult<()> {
    if order_id.is_empty() {
        return Err(FireflyError::bad_request("order id required")
            .with_field("field", "orderId"));
    }
    // … domain logic …
    Ok(())
}

// In a handler:
if let Err(fe) = charge("") {
    // Use fe.status (400) and fe.to_problem() to render RFC 7807.
    assert_eq!(fe.status, 400);
    let body = serde_json::to_string(&fe.to_problem()).unwrap();
    assert!(body.contains("\"status\":400"));
}
```

## Testing

```bash
cargo test -p firefly-kernel
```

Suite covers JSON round-trip on `ProblemDetail` (with extension
flattening and exact Go wire bytes), `FireflyResult` map / and-then,
every `FireflyError` constructor, display formatting, source-chain
traversal, the clock variants, correlation-id scoping (async, sync,
nested), and Send + Sync bounds. The pyfly-parity suites port
`tests/domain/` (specification combinators, entity identity,
pending-events raise/snapshot/drain, event auto-id/timestamp, domain
error codes and fields) and the kernel-scoped slice of
`tests/context/` (request-id / tenant-id scoping, nesting, task
isolation, header names).
