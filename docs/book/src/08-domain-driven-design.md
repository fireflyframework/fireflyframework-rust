# Domain-Driven Design

`firefly-kernel::ddd` is a zero-dependency DDD building-block kit: a
`Specification` predicate algebra, an `Entity` trait with identity equality,
transient domain events, and an aggregate's pending-events buffer. This chapter
models a small `Wallet` aggregate to show how the pieces fit, and how they
relate to repositories and events.

> **Spring parity** — This is the same vocabulary you know from DDD on the JVM —
> entities, value objects, aggregates, specifications, domain events — expressed
> as idiomatic Rust traits. There is no framework lock-in: it is plain types.

## Value objects

A value object is identity-free and compared by value. In Rust that is just a
`Clone` struct (use struct-update syntax to "modify" it by producing a new one):

```rust
#[derive(Clone, PartialEq, Debug)]
struct Money {
    amount_cents: i64,
    currency: String,
}

impl Money {
    fn add(&self, other: &Money) -> Money {
        Money {
            amount_cents: self.amount_cents + other.amount_cents,
            currency: self.currency.clone(),
        }
    }
}
```

## Entities and identity

An `Entity` has identity — two entities are "the same" if their ids match, even
if their fields differ. Implement the `Entity` trait to get `is_transient` and
`same_identity` for free:

```rust
use firefly_kernel::ddd::Entity;

struct Wallet {
    id: Option<String>,   // None while transient (not yet persisted)
    balance_cents: i64,
}

impl Entity for Wallet {
    type Id = String;
    fn id(&self) -> Option<&String> {
        self.id.as_ref()
    }
}

let a = Wallet { id: Some("w1".into()), balance_cents: 0 };
let b = Wallet { id: Some("w1".into()), balance_cents: 9999 };
assert!(a.same_identity(&b));     // same id => same entity
assert!(!a.is_transient());

let fresh = Wallet { id: None, balance_cents: 0 };
assert!(fresh.is_transient());    // no id yet
```

## Specifications — a predicate algebra

A `Specification<T>` is a reusable, composable business rule. Implement
`is_satisfied_by`, then combine rules with `.and()`, `.or()`, and `.not()`. Any
`Fn(&T) -> bool` closure is already a `Specification`, so you can mix named
rules with ad-hoc ones:

```rust
use firefly_kernel::ddd::Specification;

struct Customer { age: u32, premium: bool }

struct IsAdult;
impl Specification<Customer> for IsAdult {
    fn is_satisfied_by(&self, c: &Customer) -> bool {
        c.age >= 18
    }
}

// Combine a named spec with an inline closure spec.
let premium_adult = IsAdult.and(|c: &Customer| c.premium);

assert!(premium_adult.is_satisfied_by(&Customer { age: 30, premium: true }));
assert!(!premium_adult.is_satisfied_by(&Customer { age: 30, premium: false }));
```

Specifications shine for query filters and guard clauses: define the rule once,
reuse it in validation, in repository filtering, and in tests.

## Domain events

A transient domain event records "something happened" — it is collected during
a transaction and dispatched after the unit of work commits. Each event embeds
an `EventMeta` (a fresh UUID v4 + a UTC timestamp); `event_type` defaults to the
short type name:

```rust
use firefly_kernel::ddd::{EventMeta, TransientDomainEvent};

struct WalletCredited {
    meta: EventMeta,
    wallet_id: String,
    amount_cents: i64,
}

impl WalletCredited {
    fn new(wallet_id: String, amount_cents: i64) -> Self {
        Self { meta: EventMeta::new(), wallet_id, amount_cents }
    }
}

impl TransientDomainEvent for WalletCredited {
    fn meta(&self) -> &EventMeta {
        &self.meta
    }
}

let ev = WalletCredited::new("w1".into(), 500);
assert_eq!(ev.event_type(), "WalletCredited"); // defaults to the type name
```

> **Note** — This is the **non-event-sourced** event: state persists through
> repositories and events are merely collected for post-commit publication. The
> event-sourced variant (versioned, wire-formatted, `EventStore`-coupled) lives
> in `firefly-eventsourcing` — see [Event Sourcing](./11-event-sourcing.md).

## The aggregate root

An aggregate enforces invariants and accumulates the events it raises.
`PendingEvents<E>` is the buffer — `raise` to record, `pending` to inspect,
`drain` to take-and-clear after committing:

```rust
use firefly_kernel::ddd::{EventMeta, PendingEvents, TransientDomainEvent};

struct WalletCredited { meta: EventMeta, amount_cents: i64 }
impl TransientDomainEvent for WalletCredited {
    fn meta(&self) -> &EventMeta { &self.meta }
}

struct Wallet {
    id: String,
    balance_cents: i64,
    events: PendingEvents<WalletCredited>,
}

impl Wallet {
    fn new(id: String) -> Self {
        Self { id, balance_cents: 0, events: PendingEvents::new() }
    }

    // A behaviour method: enforce the invariant, mutate state, raise an event.
    fn credit(&mut self, amount_cents: i64) -> Result<(), String> {
        if amount_cents <= 0 {
            return Err("credit must be positive".into());
        }
        self.balance_cents += amount_cents;
        self.events.raise(WalletCredited { meta: EventMeta::new(), amount_cents });
        Ok(())
    }
}

let mut w = Wallet::new("w1".into());
w.credit(500).unwrap();
w.credit(250).unwrap();
assert_eq!(w.balance_cents, 750);
assert_eq!(w.events.len(), 2);

// After persisting, drain the events to publish them on the broker.
let to_publish = w.events.drain();
assert_eq!(to_publish.len(), 2);
assert!(w.events.is_empty());
```

## The application-service workflow

The shape of a write use case in a DDD service:

1. **Load** the aggregate from a repository (`Repository<T, K>::find_by_id`).
2. **Invoke** a behaviour method that enforces invariants and `raise`s events.
3. **Save** the aggregate (`Repository::save` — or `next_id` from a fresh
   `Uuid::new_v4()` for a brand-new one).
4. **Drain** the pending events and **publish** them on the
   [EDA broker](./10-eda-messaging.md) after the unit of work commits.

```rust,ignore
// Pseudocode for the application service.
let mut wallet = repo.find_by_id(&id).await?;     // 1. load
wallet.credit(500)?;                               // 2. behaviour + raise
repo.save(wallet.clone()).await?;                  // 3. save
for ev in wallet.events.drain() {                  // 4. publish post-commit
    broker.publish(to_event(&ev)).await?;
}
```

This keeps the domain pure (no I/O in the aggregate), the application service
thin (orchestration only), and events published exactly once after commit.

Mapping the rest of the DDD glossary to Firefly:

| DDD concept           | Firefly                                              |
|-----------------------|------------------------------------------------------|
| Value object          | a `Clone` struct + struct-update syntax              |
| Entity                | `firefly_kernel::ddd::Entity`                        |
| Aggregate root        | a struct holding `PendingEvents<E>`                  |
| Specification         | `firefly_kernel::ddd::Specification<T>`              |
| Domain event          | `firefly_kernel::ddd::TransientDomainEvent`          |
| Domain repository     | `firefly_data::Repository<T, K>`                     |
| Business-rule error   | `FireflyError::business_rule(rule, detail)` (422)    |
| Aggregate-not-found   | `FireflyError::aggregate_not_found(type, id)` (404)  |

Next, separate reads from writes with [CQRS](./09-cqrs.md).
