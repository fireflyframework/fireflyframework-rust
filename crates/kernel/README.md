# `firefly-kernel`

> **Tier:** Foundational · **Status:** Stable

## Overview

`firefly-kernel` is the **shared-vocabulary tier** of the framework. It
exposes the four primitives every Firefly crate agrees on:

1. The RFC 7807 [`ProblemDetail`](#problemdetail-rfc-7807) envelope.
2. The [`FireflyResult<T>`](#fireflyresultt) success-or-failure alias.
3. The [`Clock`](#clock) abstraction for testable time.
4. The [`FireflyError`](#fireflyerror--helpers) typed error family.

Every method in every other crate returns one of these types, giving
the whole platform a single, stable wire shape: any Firefly service
running a given version emits the same JSON.

## Why a separate crate?

Rust's `std::error::Error` trait is intentionally minimal — which means
any framework that wants typed error codes / structured fields / HTTP
status mapping has to define its own. `firefly-kernel` provides the
canonical type so the whole platform agrees on one typed error shape
and one wire format.

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
on key collision with extensions. Serialization is deterministic: keys
are lexicographically ordered and strings carry HTML-safe escaping —
`<`, `>`, `&` and U+2028/U+2029 are written as the u003c, u003e, u0026,
u2028 and u2029 Unicode escapes.
Constructors emit the canonical type URIs
(`https://fireflyframework.org/problems/<kind>`):
`ProblemDetail::bad_request`, `unauthorized`, `forbidden`, `not_found`,
`conflict`, `unprocessable`, `rate_limited`, `internal`, `validation`.

### `FireflyResult<T>`

The framework's success-or-failure alias builds directly on Rust's
native result type:

```rust,ignore
pub type FireflyResult<T> = Result<T, FireflyError>;
```

`map`, `and_then`, and the `?` operator are the idiomatic combinators —
no bespoke envelope or accessor helpers are needed.

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
values whose `source()` chain integrates with the standard
`std::error::Error` machinery for downcasting and inspection. Helpers:
`is_firefly(&err)`, `status_of(&err)`,
`as_problem(&err)` (renders any `std::error::Error` as a
`ProblemDetail`) — each walks the full source chain.

### Correlation context

```rust,ignore
let out = with_correlation_id("abc-123", async {
    correlation_id() // Some("abc-123")
}).await;
let fresh = new_correlation_id(); // 32-char hex
```

Correlation ids propagate through a tokio task-local scope; nested
scopes shadow the enclosing value. `HEADER_CORRELATION_ID`
(`X-Correlation-Id`) and `HEADER_IDEMPOTENCY_KEY` (`Idempotency-Key`)
are exported for cross-crate agreement.

### Version

`firefly_kernel::VERSION` is the released framework version
(`"26.6.7"` at the time of writing — CalVer expressed as valid semver)
— embedded in the actuator `/version` payload and the startup banner.

## Domain building blocks and structured context

The kernel layers four additional surfaces on top of its four core
primitives, all additive.

### `ddd` module — domain building blocks

`firefly_kernel::ddd` (every item also re-exported at the crate root)
is the zero-dependency DDD building-block kit:

- `Specification<T>` — the predicate object with an `is_satisfied_by`
  method, plus `.and()` / `.or()` / `.not()` combinators
  (`AndSpec` / `OrSpec` / `NotSpec`); a blanket impl lets any
  `Fn(&T) -> bool` act as a `Specification`.
- `Entity` trait — identity-based equality via `id()`,
  `is_transient()`, and `same_identity()`.
- `PendingEvents<E>` — the aggregate event buffer with
  `raise` / `pending` / `drain`.
- `EventMeta` — per-event metadata: an `event_id` (UUID v4) and an
  `occurred_at` UTC timestamp.
- `TransientDomainEvent` — a domain event whose `event_type()` defaults
  to the short type name.
- `BoxedDomainEvent` — an untyped, heterogeneous event buffer.

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
`EventStore`-coupled) lives in `firefly-eventsourcing`. Value objects
are expressed idiomatically with native `Clone` + struct-update syntax,
and the repository abstraction lives in `firefly_data::Repository<T, K>`
(`find_by_id`, `find`, `find_page`, `save`, and `delete`).

### Domain error constructors

`FireflyError::business_rule(rule, detail)` signals a violated business
rule: code `DOMAIN_RULE_VIOLATION`, status 422, the rule name in the
`rule` field, defaulting the detail to `Business rule violated: <rule>`.
`FireflyError::aggregate_not_found(aggregate_type, id)` signals a
missing aggregate: code `DOMAIN_AGGREGATE_NOT_FOUND`, status 404, with
structured `aggregate_type` / `id` fields.

### Request-id and tenant-id scopes

Two further task-local scopes complement the correlation id, carrying
per-request observability context:

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

### Typed structured-error model (`ErrorResponse`)

`ErrorResponse` is a classification-rich error model **additive over**
`ProblemDetail` (it does not touch the `application/problem+json`
bytes). It adds first-class `ErrorCategory` / `ErrorSeverity` enums,
`retryable` / `retry_after`, tracing ids (`trace_id` / `span_id` /
`transaction_id`), and per-field `FieldError`s.

```rust,ignore
use firefly_kernel::{ErrorCategory, ErrorResponse, ErrorSeverity, FieldError};

let resp = ErrorResponse::new("2026-06-12T00:00:00Z", 422, "Validation Error",
        "Input validation failed", "VALIDATION_ERROR", "/api/users")
    .with_category(ErrorCategory::Validation)
    .with_severity(ErrorSeverity::Low)
    .with_field_error(FieldError::new("email", "Invalid").with_rejected_value("nope"));

let v = resp.to_value();           // canonical structured-error shape
assert_eq!(v["category"], "VALIDATION");
assert_eq!(v["retryable"], false); // category/severity/retryable always present
assert!(v.get("trace_id").is_none()); // unset optionals omitted
```

`to_value()` and `Serialize` produce a stable shape: the six core
members plus `category`/`severity`/`retryable` are always present;
every other optional is omitted when unset or empty; keys use
`snake_case` names (`field_errors`, `retry_after`, …) — **not** the
`ProblemDetail` wire keys. `ErrorCategory` defaults to `Technical` and
`ErrorSeverity` to `Medium`. Pick the model whose wire contract you
need: `problem+json` clients consume `ProblemDetail`; consumers that
want classification-rich, field-level diagnostics consume
`ErrorResponse`.

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
flattening and exact, byte-for-byte wire output), `FireflyResult` map / and-then,
every `FireflyError` constructor, display formatting, source-chain
traversal, the clock variants, correlation-id scoping (async, sync,
nested), and Send + Sync bounds. The domain suites exercise the `ddd`
building blocks (specification combinators, entity identity,
pending-events raise/snapshot/drain, event auto-id/timestamp, domain
error codes and fields) and the context suites cover request-id /
tenant-id scoping, nesting, task isolation, and header names.
