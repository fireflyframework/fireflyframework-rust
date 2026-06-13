# Domain-Driven Design

Lumen can open wallets and read them back, and it has a place to put the read
model. But look closely and something is missing: nothing yet *owns* the rules.
Where is "you cannot withdraw more than you hold"? Where is "an amount must be
positive"? Where is "a wallet must have an owner"? Right now those would live as
scattered `if`-statements in a handler — exactly the kind of rule a future
developer can bypass by writing to the store directly.

**Domain-Driven Design** fixes that by making the model responsible for its own
invariants. This chapter builds Lumen's domain core: the `Money` **value
object** (immutable, integer cents, exact arithmetic) and the `Wallet`
**aggregate** that guards the overdraft, positive-amount, and owner-required
rules — each command raising the domain event that records what happened. Both
files are drawn verbatim from `samples/lumen`.

> **By the end of this chapter, Lumen will** have `src/money.rs` and the heart
> of `src/domain.rs`: a `Money` value object closed under `add` / `subtract`, a
> `Wallet` aggregate with `open` / `deposit` / `withdraw` that enforce the
> invariants and `raise` events, and a typed `DomainError` family whose `Display`
> strings surface verbatim as RFC 9457 problem details. The aggregate carries
> `#[derive(AggregateRoot)]`; nothing carries `thiserror`.

> **Spring parity.** This is the DDD vocabulary you know from the JVM — value
> objects, aggregates, aggregate roots, domain events — expressed as idiomatic
> Rust. `#[derive(AggregateRoot)]` is the analog of jMolecules's
> `@AggregateRoot` / Spring Data's `AbstractAggregateRoot`, generating the
> event-buffer plumbing so your code holds only the rules.

## The `Money` value object

A value object is defined entirely by its attributes — it has no identity — and
it is **immutable**: every operation returns a *new* value rather than mutating
in place. Money is the textbook example, and getting it right matters more here
than almost anywhere: amounts are stored as integer **minor units** (cents), so
the arithmetic is exact. No binary floating-point drift, the classic correctness
bug a money type exists to prevent.

Here is Lumen's `Money`, the whole core of `src/money.rs`:

```rust,ignore
// samples/lumen/src/money.rs
use std::fmt;
use serde::{Deserialize, Serialize};

/// An exact monetary amount, expressed in integer minor units (cents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money {
    cents: i64,
}

impl Money {
    /// A zero amount — the opening balance of a brand-new wallet.
    pub const ZERO: Money = Money { cents: 0 };

    /// Builds a `Money` from a raw minor-unit (cent) count.
    pub const fn cents(cents: i64) -> Self {
        Money { cents }
    }

    /// Builds a `Money` from a whole-currency unit count (`from_units(10)` is €10.00).
    pub const fn from_units(units: i64) -> Self {
        Money { cents: units * 100 }
    }

    /// The amount in minor units (cents) — the wire representation.
    pub const fn cents_value(self) -> i64 {
        self.cents
    }

    /// Whether this amount is strictly positive (`> 0`).
    pub const fn is_positive(self) -> bool {
        self.cents > 0
    }

    /// Returns a new `Money` that is `self + other` (immutable addition).
    #[must_use]
    pub const fn add(self, other: Money) -> Money {
        Money { cents: self.cents + other.cents }
    }

    /// Returns `self - other`, or `MoneyError::Overdraw` if that would go below zero.
    pub fn subtract(self, other: Money) -> Result<Money, MoneyError> {
        if other.cents > self.cents {
            return Err(MoneyError::Overdraw);
        }
        Ok(Money { cents: self.cents - other.cents })
    }

    /// Validates that this amount is strictly positive, returning it unchanged on success.
    pub fn require_positive(self) -> Result<Money, MoneyError> {
        if self.is_positive() {
            Ok(self)
        } else {
            Err(MoneyError::NonPositive)
        }
    }
}
```

Several design choices are load-bearing:

- **Integer cents, never a float.** The single field is `cents: i64`, kept
  private so the only way to a `Money` is through a constructor. €10.00 is
  `Money::cents(1_000)`; €12.50 after an addition is `Money::cents(1_250)`. The
  math is exact by construction.
- **Immutable.** `add` is `#[must_use]` and `const`; it returns a fresh `Money`
  and leaves the operands untouched. So does `subtract`. There is no
  `add_assign` — a value object is replaced, not edited.
- **Closed under the wallet's operations.** `add` for credits, `subtract`
  (fallible, guarding against overdraw) for debits, and `require_positive` for
  the guard every mutating command runs before raising an event.
- **`#[serde(transparent)]`.** `Money` serializes as the bare cent integer — a
  balance of €10.00 is the JSON number `1000`, the contract the read model and
  the event payloads share. No `{ "cents": 1000 }` wrapper.

`Display` renders the human form (`1250` cents → `"12.50"`) for logs and the
banner, and the error type is hand-written:

```rust,ignore
/// The typed error a `Money` operation can fail with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoneyError {
    /// An amount was expected to be strictly positive (`> 0`) but was not.
    NonPositive,
    /// A subtraction would drop the balance below zero.
    Overdraw,
}

impl fmt::Display for MoneyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoneyError::NonPositive => f.write_str("amount must be positive"),
            MoneyError::Overdraw => f.write_str("amount exceeds balance"),
        }
    }
}

impl std::error::Error for MoneyError {}
```

> **One-dependency note.** `MoneyError` hand-writes `Display` and
> `std::error::Error` rather than deriving them with `thiserror`. That is on
> purpose: Lumen depends on exactly one Firefly crate plus `axum` and `serde`,
> and the book keeps that promise *all the way down* — even the error enums. Two
> trait impls per error type is a small price for an honest dependency list.

> **Spring parity.** A frozen, value-equal `Money` maps to a Java `record` (the
> JVM's value type) or jMolecules's `@ValueObject`. The "minor units, never a
> float" discipline is the same one Spring teams reach for `BigDecimal` or
> integer cents to enforce — Rust just makes the immutability a compiler
> guarantee.

## The `Wallet` aggregate

`Money` solves *representation*. The `Wallet` aggregate owns *behavior*: it is
the consistency boundary, the single entry point through which every change to a
wallet must flow, so the invariants cannot be bypassed.

Lumen's `Wallet` is event-sourced (the full event-store machinery arrives in
[Event Sourcing](./11-event-sourcing.md)), but the DDD shape is visible already.
The aggregate embeds the framework's `AggregateRoot` — the uncommitted-event
buffer plus a version — and `#[derive(AggregateRoot)]` generates the
`AGGREGATE_TYPE` constant and the `aggregate()` / `aggregate_mut()` accessors:

```rust,ignore
// samples/lumen/src/domain.rs
use firefly::eventsourcing::{AggregateRoot, DomainEvent};
use firefly::prelude::*;

use crate::money::{Money, MoneyError};

/// The aggregate-type discriminator stamped onto every event a Wallet raises.
pub const AGGREGATE_TYPE: &str = "Wallet";

/// The event-sourced wallet aggregate.
#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    /// The framework aggregate root — uncommitted-event buffer + version.
    pub root: AggregateRoot,
    /// The owner's display name.
    pub owner: String,
    /// The current balance as a `Money` value object.
    pub balance: Money,
    /// Whether the wallet has been opened (an empty stream is "absent").
    pub opened: bool,
}
```

The aggregate's projected state — `owner`, `balance`, `opened` — is the *result*
of applying its events. The behavior methods enforce the rules.

### The factory: `open`

`open` is the sole way to bring a wallet into existence. It validates inputs,
constructs the aggregate, and `raise`s the opening event:

```rust,ignore
impl Wallet {
    /// Opens a fresh wallet, raising a `WalletOpened` event.
    pub fn open(
        id: impl Into<String>,
        owner: impl Into<String>,
        opening_balance: Money,
    ) -> Result<Self, DomainError> {
        let id = id.into();
        let owner = owner.into();
        if owner.trim().is_empty() {
            return Err(DomainError::OwnerRequired);
        }
        if opening_balance.cents_value() < 0 {
            return Err(DomainError::NonPositiveAmount);
        }
        let mut wallet = Wallet {
            root: AggregateRoot::new(&id, AGGREGATE_TYPE),
            owner: owner.clone(),
            balance: Money::ZERO,
            opened: false,
        };
        wallet.raise(
            WalletOpened::EVENT_TYPE,
            &WalletOpened { wallet_id: id, owner, opening_balance: opening_balance.cents_value() },
        );
        wallet.balance = opening_balance;
        wallet.opened = true;
        Ok(wallet)
    }
}
```

Two invariants are enforced before any event is raised: the owner must be
non-blank (`OwnerRequired`), and the opening balance must not be negative (a
*zero* opening balance is allowed). Using a factory rather than a public
constructor guarantees the `WalletOpened` event is never forgotten — there is no
back-channel that produces a wallet without recording its birth.

### The behavior methods: `deposit` and `withdraw`

The two mutating commands follow one shape: check the wallet exists, validate
the amount, apply the `Money` operation, raise the event, update state. The
overdraft rule is enforced exactly once — by `Money::subtract`:

```rust,ignore
impl Wallet {
    /// Credits `amount` to the wallet, raising a `MoneyDeposited` event.
    pub fn deposit(&mut self, amount: Money) -> Result<(), DomainError> {
        self.require_opened()?;
        let amount = amount.require_positive()?;
        self.raise(
            MoneyDeposited::EVENT_TYPE,
            &MoneyDeposited { wallet_id: self.root.id.clone(), amount: amount.cents_value() },
        );
        self.balance = self.balance.add(amount);
        Ok(())
    }

    /// Debits `amount` from the wallet, raising a `MoneyWithdrawn` event.
    pub fn withdraw(&mut self, amount: Money) -> Result<(), DomainError> {
        self.require_opened()?;
        let amount = amount.require_positive()?;
        let remaining = self.balance.subtract(amount)?; // Overdraw → InsufficientFunds
        self.raise(
            MoneyWithdrawn::EVENT_TYPE,
            &MoneyWithdrawn { wallet_id: self.root.id.clone(), amount: amount.cents_value() },
        );
        self.balance = remaining;
        Ok(())
    }

    fn require_opened(&self) -> Result<(), DomainError> {
        if self.opened {
            Ok(())
        } else {
            Err(DomainError::NotFound(self.root.id.clone()))
        }
    }
}
```

Read `withdraw` carefully — it is where the consistency boundary earns its keep.
The order is *validate, then mutate*: `require_opened`, `require_positive`, and
the `subtract` overdraft check all run **before** the event is raised. If the
withdrawal would overdraw, `Money::subtract` returns `MoneyError::Overdraw`, the
`?` converts it to `DomainError::InsufficientFunds` (via the `From` impl below),
and the method returns *without raising anything*. The aggregate's invariant —
"balance never goes below zero" — is physically unreachable from outside,
because the only path to a withdrawal runs this gauntlet first.

### The typed `DomainError` family

The errors are a closed enum with stable `Display` strings — stable because
tests assert on them and they surface verbatim as the RFC 9457 problem `detail`
once mapped at the HTTP boundary (see [CQRS](./09-cqrs.md) and
[First HTTP API](./06-first-http-api.md)):

```rust,ignore
/// The typed domain-error family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// A command referenced an amount that was not strictly positive.
    NonPositiveAmount,
    /// A withdrawal (or transfer debit) exceeded the available balance.
    InsufficientFunds,
    /// A command targeted a wallet that was never opened.
    NotFound(String),
    /// The owner name was empty when opening a wallet.
    OwnerRequired,
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DomainError::NonPositiveAmount => f.write_str("amount must be positive"),
            DomainError::InsufficientFunds => f.write_str("insufficient funds"),
            DomainError::NotFound(id) => write!(f, "wallet {id} not found"),
            DomainError::OwnerRequired => f.write_str("owner is required"),
        }
    }
}

impl std::error::Error for DomainError {}

impl From<MoneyError> for DomainError {
    fn from(e: MoneyError) -> Self {
        match e {
            MoneyError::NonPositive => DomainError::NonPositiveAmount,
            MoneyError::Overdraw => DomainError::InsufficientFunds,
        }
    }
}
```

The `From<MoneyError> for DomainError` impl is what lets `withdraw` write
`self.balance.subtract(amount)?` and have an overdraw surface as
`InsufficientFunds` — the value object reports the arithmetic fact, the
aggregate translates it into domain language. Like `MoneyError`, `DomainError`
hand-writes its `Display` and `Error` impls: no `thiserror`, one dependency.

> **Spring parity.** Where a Spring `Wallet` aggregate would throw a
> `BusinessRuleViolation` (mapped to HTTP 422) and an `AggregateNotFound`
> (mapped to 404), Lumen returns a typed `DomainError`. `InsufficientFunds` /
> `NonPositiveAmount` / `OwnerRequired` become 422 problems and `NotFound`
> becomes a 404 — the same status mapping, decided by a `match` at the web
> boundary instead of an exception-to-status table.

## The read-model view

The aggregate is the *write* model. What `GET /api/v1/wallets/:id` returns is a
flat, query-optimized `WalletView` — the read model the previous chapter framed
as a repository. The aggregate produces it on demand:

```rust,ignore
impl Wallet {
    /// The current read-model view of this aggregate.
    pub fn view(&self) -> WalletView {
        WalletView {
            id: self.root.id.clone(),
            owner: self.owner.clone(),
            balance: self.balance.cents_value(),
            version: self.root.version,
        }
    }
}

/// The read-model projection of a wallet — the wire shape served by
/// `GET /api/v1/wallets/:id` and stored in the read-model repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletView {
    pub id: String,
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied).
    pub version: i64,
}
```

Keeping `Wallet` (rich, rule-enforcing, event-raising) and `WalletView` (flat,
serializable, no behavior) as separate types is the same domain/persistence
separation the previous chapter drew around the repository: the aggregate never
serializes itself, and the wire shape never carries an invariant. The `version`
field lets a client detect staleness under the eventual consistency CQRS
introduces.

## Proving the invariants

Because the aggregate is a plain struct with plain methods, you can exercise
every rule with no database and no HTTP — the unit tests that ship in
`samples/lumen/src/domain.rs`:

```rust,ignore
#[test]
fn open_validates_owner_and_balance() {
    assert_eq!(Wallet::open("w1", "  ", Money::cents(100)).unwrap_err(), DomainError::OwnerRequired);
    assert_eq!(Wallet::open("w1", "alice", Money::cents(-1)).unwrap_err(), DomainError::NonPositiveAmount);
    let w = Wallet::open("w1", "alice", Money::ZERO).unwrap();
    assert!(w.opened);
    assert_eq!(w.balance, Money::ZERO);
}

#[test]
fn withdraw_rejects_overdraft() {
    let mut w = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
    assert_eq!(w.withdraw(Money::cents(101)).unwrap_err(), DomainError::InsufficientFunds);
    // The failed command raised no event beyond the open.
    assert_eq!(w.root.uncommitted().len(), 1);
}

#[test]
fn deposit_and_withdraw_update_balance_and_raise_events() {
    let mut w = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
    w.deposit(Money::cents(50)).unwrap();
    assert_eq!(w.balance, Money::cents(150));
    w.withdraw(Money::cents(30)).unwrap();
    assert_eq!(w.balance, Money::cents(120));
}
```

The `withdraw_rejects_overdraft` test makes the consistency boundary concrete:
after a rejected withdrawal, the aggregate's uncommitted-event buffer still holds
exactly one event — the `WalletOpened` from the factory. The overdraft never
produced a `MoneyWithdrawn`, so nothing partial can ever be persisted. That is
the difference between a service-level guard (a convention) and an aggregate
invariant (a physical constraint): the model exposes no mechanism to violate it.

## What changed in Lumen

Lumen now has a domain core that owns its rules:

- **`src/money.rs`** — a `Money` value object: immutable, integer cents,
  `#[serde(transparent)]` so it rides the wire as a bare number, closed under
  `add` / `subtract` / `require_positive`, with a hand-written `MoneyError`
  (no `thiserror`).
- **`src/domain.rs`** — the `Wallet` aggregate carrying
  `#[derive(AggregateRoot)]` (which generates `AGGREGATE_TYPE` and the
  `aggregate()` accessors). `open` / `deposit` / `withdraw` enforce the three
  invariants — owner required, amounts positive, no overdraft — *before* raising
  an event, so a rejected command leaves the event buffer untouched.
- **The typed `DomainError`** family with stable `Display` strings that surface
  as RFC 9457 problem details, plus the `From<MoneyError>` bridge that turns an
  arithmetic overdraw into `InsufficientFunds`.
- **`WalletView`** — the flat read-model projection the aggregate hands out via
  `view()`, kept a separate type from the rule-enforcing aggregate.

The event payloads (`WalletOpened` / `MoneyDeposited` / `MoneyWithdrawn`) and
the `rehydrate` / `apply` fold that reconstruct a wallet from its stream get
their full treatment in [Event Sourcing](./11-event-sourcing.md). Here, the
shape that matters is the DDD one: a value object you cannot corrupt and an
aggregate you cannot bypass.

## Exercises

1. **Break an invariant, watch it hold.** In a `#[cfg(test)]` block, open a
   wallet with `Money::cents(100)` and call `withdraw(Money::cents(200))`. Assert
   the error is `DomainError::InsufficientFunds` *and* that
   `w.root.uncommitted().len()` is still `1` — proving the rejected command
   raised no event.

2. **Add a `transfer_within` rule (domain only).** Write a free function
   `fn transfer(from: &mut Wallet, to: &mut Wallet, amount: Money) ->
   Result<(), DomainError>` that calls `from.withdraw(amount)?` then
   `to.deposit(amount)?`. Test that a transfer exceeding the source balance
   fails on the withdraw leg and leaves the *target* balance unchanged. (The
   real, persisted version becomes the saga in [Sagas](./12-sagas.md).)

3. **Confirm the wire shape.** Serialize a freshly opened wallet's `view()` with
   `serde_json` and assert it equals
   `{"id":"wlt_1","owner":"alice","balance":250,"version":1}` — verifying that
   `Money`'s `#[serde(transparent)]` and `WalletView`'s field order produce the
   contract the read model and clients share.

4. **Justify the hand-written error.** In two sentences, explain why
   `MoneyError` and `DomainError` implement `Display` / `Error` by hand instead
   of deriving with `thiserror`, and what it would cost the book's
   one-dependency promise to add the crate.

Next, separate the write path from the read path with the command/query bus. See
[CQRS](./09-cqrs.md).
