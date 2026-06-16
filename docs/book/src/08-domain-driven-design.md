# Domain-Driven Design

Lumen can already open wallets and read them back, and it has a place to put the
read model. But look closely and something is missing: nothing yet *owns* the
rules. Where is "you cannot withdraw more than you hold"? Where is "an amount
must be positive"? Where is "a wallet must have an owner"? Right now those would
live as scattered `if`-statements in a handler — exactly the kind of rule a
future developer can bypass by writing to the store directly.

**Domain-Driven Design (DDD)** fixes that by making the model responsible for
its own invariants. In this chapter you build Lumen's domain core from first
principles: the `Money` *value object* (immutable, integer cents, exact
arithmetic) and the `Wallet` *aggregate* that guards the overdraft,
positive-amount, and owner-required rules — each command raising the domain
event that records what happened. Both files are drawn verbatim from
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen),
so the crate you grow here matches the finished service line for line.

This is pure Rust modelling: there is no HTTP, no database, and no framework
runtime in the hot path. The framework contributes exactly two derives —
`#[derive(AggregateRoot)]` and `#[derive(DomainEvent)]` — and otherwise stays
out of your way, which is the point: your rules live in plain methods you can
unit-test with no I/O at all.

By the end of this chapter you will:

- Tell a **value object** apart from an **entity / aggregate**, and know why
  `Money` is the former and `Wallet` the latter.
- Build a `Money` value object that is immutable, stored as integer cents, and
  closed under the operations a wallet needs (`add` / `subtract` /
  `require_positive`).
- Build the `Wallet` aggregate so its overdraft, positive-amount, and
  owner-required invariants are *physically* unreachable from outside —
  validated before any event is raised.
- Use `#[derive(AggregateRoot)]` and `#[derive(DomainEvent)]` to embed the
  framework's event buffer and stamp typed events, writing only the rules
  yourself.
- Map domain failures to a typed `DomainError` family whose `Display` strings
  surface verbatim as RFC 9457 problem details.
- Prove every invariant with plain unit tests — no database, no HTTP.

## Concepts you will meet

Before the first line of code, here are the DDD ideas this chapter leans on.
Each is reintroduced in context where it is first used; this is the short
version.

> **Note** **Key term — value object.** A *value object* is a domain type
> defined entirely by its attributes — it has **no identity** — and is
> **immutable**: every operation returns a *new* value instead of mutating in
> place. Two value objects with equal attributes are equal, full stop. `Money`
> is the textbook example. The Java/DDD analog is exactly a value object (a
> JPA `@Embeddable`, or a Java `record` used as a value).

> **Note** **Key term — entity and aggregate.** An *entity* has an **identity**
> that persists across changes (a wallet stays "the same wallet" as its balance
> moves). An *aggregate* is a cluster of entities and value objects treated as
> one unit, with a single **aggregate root** as its sole entry point — the
> consistency boundary through which every change must flow. `Wallet` is the
> aggregate root here. In Spring/JPA terms this is the `@Entity` that owns its
> children and guards its invariants.

> **Note** **Key term — domain event.** A *domain event* is an immutable record
> of something that happened in the domain, in the past tense (`WalletOpened`,
> `MoneyDeposited`). The aggregate *raises* one whenever it changes state, so
> the change is captured as a fact rather than left implicit. This is the same
> notion as a Spring `ApplicationEvent` published from a domain method, but here
> the events are also the persisted source of truth (you will see that fully in
> [Event Sourcing](./11-event-sourcing.md)).

> **Note** **Key term — invariant.** An *invariant* is a rule that must hold for
> the model to be valid — "balance never goes below zero", "owner is never
> blank". An aggregate's job is to make its invariants impossible to violate
> from outside. There is no Spring annotation for this; it is the discipline the
> aggregate boundary exists to enforce.

The chapter builds two files: `src/money.rs` (the value object) and
`src/domain.rs` (the aggregate, its events, the error family, and the read-model
view). You declared both in the `mod` list back in
[Quickstart](./02-quickstart.md), so nothing in `main.rs` changes — you are
filling modules the entry point already names.

## Step 1 — Define the `Money` value object's shape

Start with representation. `Money` solves *how an amount is stored and
compared*; the `Wallet` aggregate (Step 5 onward) will solve *behavior*. Getting
the representation right matters more here than almost anywhere: amounts are
stored as integer **minor units** (cents), so the arithmetic is exact — no
binary floating-point drift, the classic correctness bug a money type exists to
prevent.

Create `src/money.rs` and declare the struct and its imports:

```rust,ignore
// samples/lumen/src/money.rs
use std::fmt;

use serde::{Deserialize, Serialize};

/// An exact monetary amount, expressed in integer minor units (cents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money {
    /// The amount in minor units (cents). Kept private so the only way to a
    /// `Money` is through the validating constructors.
    cents: i64,
}
```

What just happened, decision by decision:

- **Integer cents, never a float.** The single field is `cents: i64`, kept
  *private* so the only way to a `Money` is through a constructor. €10.00 is
  `Money::cents(1_000)`; €12.50 after an addition is `Money::cents(1_250)`. The
  math is exact by construction — there is no `f64` anywhere to drift.
- **A value object is compared by value.** The `PartialEq, Eq, PartialOrd, Ord`
  derives make two `Money`s equal exactly when their cents are equal, and
  orderable so the wallet can ask "is this amount more than the balance?".
- **`Copy`, because it is a value.** `Money` is a single `i64`, so it copies
  freely; you never juggle references to it.
- **`#[serde(transparent)]` is the wire contract.** `Money` serializes as the
  *bare cent integer* — a balance of €10.00 is the JSON number `1000`, not
  `{ "cents": 1000 }`. That is the contract the read model and the event
  payloads share, and it is why the field can stay private without hurting the
  wire shape.

> **Note** **Key term — minor units.** *Minor units* are the smallest
> indivisible unit of a currency — cents for euros and dollars. Storing money as
> an integer count of minor units (1000 cents, not 10.00 euros) keeps arithmetic
> exact. This is the same discipline as a database `BIGINT` cents column or a
> Java `long` of minor units.

> **Tip** **Checkpoint.** `src/money.rs` exists with a `Money` struct holding one
> private `cents: i64` field. It will not compile yet — the constructors come
> next — but the shape is locked in.

## Step 2 — Give `Money` immutable, validating operations

A value object exposes only operations that *return new values*. Add the
constructors, the accessors, and the three operations a wallet needs. Append
this `impl` block to `src/money.rs`:

```rust,ignore
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

    /// Whether this amount is zero.
    pub const fn is_zero(self) -> bool {
        self.cents == 0
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

What just happened — four design choices are load-bearing here:

- **Immutable.** `add` is `#[must_use]` and `const`; it returns a *fresh*
  `Money` and leaves the operands untouched. So does `subtract`. There is no
  `add_assign` — a value object is replaced, not edited. `#[must_use]` makes the
  compiler warn if you call `add` and forget to use the result, catching the
  "I thought this mutated in place" bug at compile time.
- **Closed under the wallet's operations.** `add` for credits, `subtract`
  (fallible, guarding against overdraw) for debits, and `require_positive` for
  the guard every mutating command runs before raising an event. "Closed" means
  every operation a wallet performs takes `Money` and yields `Money` (or a
  `MoneyError`), so amounts never leak into raw integers.
- **`subtract` is where overdraft lives.** It returns `Result<Money,
  MoneyError>`: subtracting more than you hold is `MoneyError::Overdraw`, not a
  silent negative balance. This is the *only* place the "never below zero" rule
  is checked — the aggregate reuses it rather than re-implementing it.
- **`const` where possible.** `ZERO`, `cents`, `from_units`, `cents_value`,
  `is_positive`, `is_zero`, and `add` are `const fn`, so `Money::ZERO` and
  `Money::cents(100)` can be used in const contexts. `subtract` and
  `require_positive` are not `const` because they return a `Result`.

> **Note** **Key term — `#[must_use]`.** Annotating a function with `#[must_use]`
> tells the compiler to warn when its return value is ignored. On an immutable
> operation like `add` it is the guardrail that turns "I forgot the result is a
> new value" into a compile-time warning instead of a lost update.

## Step 3 — Render and report `Money` failures by hand

Two more pieces complete the value object: a human-readable `Display`, and the
typed error its operations return. Lumen hand-writes both `Display` and
`std::error::Error` for `MoneyError` rather than deriving them — that keeps the
book's one-dependency promise honest all the way down to the error enums.

Add the error type and the two `Display` impls to `src/money.rs`:

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

impl fmt::Display for Money {
    /// Renders the amount as a fixed two-decimal major-unit string
    /// (`1250` cents → `"12.50"`), the human-readable form used in logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.cents < 0 { "-" } else { "" };
        let abs = self.cents.abs();
        write!(f, "{sign}{}.{:02}", abs / 100, abs % 100)
    }
}
```

What just happened:

- **`MoneyError` is a closed enum** with two cases — exactly the two ways a
  `Money` operation can fail. Because it derives `PartialEq, Eq`, tests can
  assert `err == MoneyError::Overdraw` directly.
- **`Display` carries the message text**, and `impl std::error::Error for
  MoneyError {}` makes it a first-class error so `?` and trait objects work. The
  empty block is enough because `Error` has default methods.
- **`Money`'s own `Display`** turns `1250` into `"12.50"` for logs and the
  banner — the major-unit form a human reads, kept separate from the
  minor-unit form the wire carries.

> **Design note.** `MoneyError` hand-writes `Display` and `std::error::Error`
> rather than deriving them with `thiserror`. That is on purpose: Lumen depends
> on exactly one Firefly crate plus `axum` and `serde`, and the book keeps that
> promise all the way down — even the error enums. Two trait impls per error type
> is a small price for an honest dependency list, and `Money` itself is a frozen,
> value-equal type that Rust makes immutable as a compiler guarantee.

> **Tip** **Checkpoint.** Run `cargo test --lib money` (or `cargo build`).
> `src/money.rs` now compiles on its own: a private-field value object, three
> operations, a two-variant error, and a `Display` that prints `"12.50"`. The
> arithmetic is exact and the type cannot be constructed into an invalid state.

## Step 4 — Set up the `Wallet` aggregate and its events

`Money` solved representation. The `Wallet` aggregate owns *behavior*: it is the
consistency boundary, the single entry point through which every change to a
wallet must flow, so the invariants cannot be bypassed.

Lumen's `Wallet` is event-sourced — every command produces a domain event, and
the wallet's state is the *result* of folding those events. The full event-store
machinery arrives in [Event Sourcing](./11-event-sourcing.md); here you build
just the DDD shape. The aggregate embeds the framework's `AggregateRoot` (an
uncommitted-event buffer plus a version), and two derives do the mechanical
work.

> **Note** **Key term — `#[derive(AggregateRoot)]`.** This derive finds the
> embedded `firefly` `AggregateRoot` field on your struct and generates an
> `AGGREGATE_TYPE` associated constant plus `aggregate()` / `aggregate_mut()`
> accessors over it. The embedded `AggregateRoot` itself carries the
> uncommitted-event buffer, the aggregate id, and the version — so your struct
> holds only the projected state and the rules. The Spring/Axon analog is an
> `@Aggregate` root that the framework manages.

> **Note** **Key term — `#[derive(DomainEvent)]`.** This derive stamps an event
> payload struct with a stable `EVENT_TYPE` discriminator (its struct name) and
> generates a `to_domain_event(...)` conversion onto the framework's wire event.
> You declare the payload as a plain serializable struct; the derive supplies the
> type tag so you never spell event names as bare string literals at the call
> sites.

Create `src/domain.rs` with its imports, the `AGGREGATE_TYPE` constant, and the
three event payloads:

```rust,ignore
// samples/lumen/src/domain.rs
use firefly::eventsourcing::{AggregateRoot, DomainEvent};
use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::money::{Money, MoneyError};

/// The aggregate-type discriminator stamped onto every event a Wallet raises.
/// `#[derive(AggregateRoot)]` also exposes it as `Wallet::AGGREGATE_TYPE`.
pub const AGGREGATE_TYPE: &str = "Wallet";

/// Payload of the event raised when a wallet is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct WalletOpened {
    pub wallet_id: String,
    pub owner: String,
    /// The opening balance, in minor units (cents).
    pub opening_balance: i64,
}

/// Payload of the event raised when money is credited to a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyDeposited {
    pub wallet_id: String,
    /// The deposited amount, in minor units (cents).
    pub amount: i64,
}

/// Payload of the event raised when money is debited from a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyWithdrawn {
    pub wallet_id: String,
    /// The withdrawn amount, in minor units (cents).
    pub amount: i64,
}
```

What just happened, line by line:

- **`use firefly::eventsourcing::{AggregateRoot, DomainEvent};`** imports the two
  framework *types* — the embeddable root struct and the wire-event struct.
- **`use firefly::prelude::*;`** brings the framework's derive macros into scope,
  including `AggregateRoot`, `DomainEvent`, and `Schema` (used by the read-model
  view in Step 8). Everything reaches you through the single `firefly` facade.
- **`AGGREGATE_TYPE`** is the string discriminator stamped onto every event the
  wallet raises. It is declared as a public constant *and* re-exposed as
  `Wallet::AGGREGATE_TYPE` by the derive, so both spellings name the same value.
- **Each event payload is past tense** (`WalletOpened`, not `OpenWallet`) and
  carries `#[derive(DomainEvent)]`. The derive gives each one a `EVENT_TYPE`
  const equal to its struct name (`"WalletOpened"`, etc.), which you use at the
  raise sites instead of typing the string by hand.

## Step 5 — Declare the aggregate root struct

Now the aggregate itself. It embeds the framework's `AggregateRoot` as a field
named `root`, carries the projected state (`owner`, `balance`, `opened`), and
derives `AggregateRoot` to generate the accessors and the `AGGREGATE_TYPE`
constant.

Add the struct to `src/domain.rs`:

```rust,ignore
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

What just happened:

- **`root: AggregateRoot`** is the embedded framework field. It holds the
  uncommitted-event buffer, the aggregate id (`root.id`), and the version
  (`root.version`). The derive locates this field by its type.
- **`#[firefly(aggregate_type = "Wallet")]`** tells the derive what string to use
  for `Wallet::AGGREGATE_TYPE`. It matches the `AGGREGATE_TYPE` constant you
  declared in Step 4 — both name `"Wallet"`.
- **`owner` / `balance` / `opened`** are the *projected state* — the result of
  applying the wallet's events. `balance` is a `Money` value object, so the
  aggregate reuses all the exact-arithmetic guarantees from Steps 1–3. `opened`
  distinguishes a real wallet from an empty event stream ("absent").
- **`Clone`** lets a handler take a working copy of a rehydrated wallet without
  touching the original — useful under the eventual consistency CQRS introduces.

> **Tip** **Checkpoint.** `cargo build` still fails — `Wallet` has no methods yet
> and `DomainError` is undefined — but the derives should resolve. If the
> compiler complains that it cannot find an `AggregateRoot` field, confirm the
> embedded field is typed exactly `AggregateRoot` (from
> `firefly::eventsourcing`), not your own type.

## Step 6 — Write the factory method: `open`

`open` is the *sole* way to bring a wallet into existence. It validates inputs,
constructs the aggregate, and `raise`s the opening event. Using a factory rather
than a public constructor guarantees the `WalletOpened` event is never forgotten
— there is no back-channel that produces a wallet without recording its birth.

Add an `impl Wallet` block with `open`:

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
            &WalletOpened {
                wallet_id: id,
                owner,
                opening_balance: opening_balance.cents_value(),
            },
        );
        wallet.balance = opening_balance;
        wallet.opened = true;
        Ok(wallet)
    }
}
```

What just happened, in order:

- **Validate first, construct second.** Two invariants are enforced *before* any
  event is raised: the owner must be non-blank (`OwnerRequired`), and the opening
  balance must not be negative (a *zero* opening balance is explicitly allowed —
  the check is `< 0`, not `<= 0`).
- **`AggregateRoot::new(&id, AGGREGATE_TYPE)`** constructs the embedded root with
  this wallet's id and its aggregate-type tag, at version 0 with an empty event
  buffer.
- **`wallet.raise(WalletOpened::EVENT_TYPE, &WalletOpened { ... })`** records the
  birth event. `WalletOpened::EVENT_TYPE` is the discriminator the
  `#[derive(DomainEvent)]` generated (the string `"WalletOpened"`), so the call
  site never spells it by hand. `raise` is a small helper you add in Step 9; it
  serializes the payload and pushes it onto `root`.
- **State is updated after the event.** `wallet.balance = opening_balance` and
  `wallet.opened = true` set the projected state to match what the event
  describes. The event is the fact; the fields are the cached projection of it.

> **Note** **Key term — factory method.** A *factory method* is a static
> (associated) function that builds a fully valid instance, instead of exposing a
> public constructor. It is the one door into the aggregate, so it can enforce
> the birth invariants and guarantee the `WalletOpened` event is always raised.
> The Spring/DDD analog is a static factory on the aggregate root (or a domain
> service that produces it).

## Step 7 — Write the behavior methods: `deposit` and `withdraw`

The two mutating commands follow one shape: check the wallet exists, validate
the amount, apply the `Money` operation, raise the event, update state. The
overdraft rule is enforced exactly once — by `Money::subtract`, which you already
built in Step 2.

Add these methods to the same `impl Wallet` block:

```rust,ignore
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
```

What just happened — read `withdraw` carefully, because it is where the
consistency boundary earns its keep:

- **The order is *validate, then mutate*.** `require_opened()`,
  `require_positive()`, and the `subtract` overdraft check all run **before** the
  event is raised. If the withdrawal would overdraw, `Money::subtract` returns
  `MoneyError::Overdraw`, the `?` converts it to `DomainError::InsufficientFunds`
  (via the `From` impl you add in Step 8), and the method returns *without
  raising anything*.
- **The invariant is unreachable from outside.** "Balance never goes below zero"
  cannot be violated, because the only path to a withdrawal runs this gauntlet
  first. There is no setter on `balance`, no way to skip `subtract`. That is the
  difference between a service-level guard (a convention someone can forget) and
  an aggregate invariant (a physical constraint).
- **`require_opened` turns "absent" into a typed error.** A command against a
  wallet that was never opened returns `DomainError::NotFound(id)`, which the web
  boundary later maps to a 404. `deposit` and `withdraw` both gate on it first.
- **`self.root.id.clone()`** reads the id off the embedded root to stamp each
  event with the wallet it belongs to.

> **Note** The whole reason an aggregate exists is to be the *only*
> way to change its state. Because `deposit` and `withdraw` take `&mut self` and
> there are no public setters, every mutation funnels through these methods and
> past their guards. A future developer cannot "just write to the store" and
> skip the rules — the rules are the door.

## Step 8 — Add the typed `DomainError` family

The errors are a closed enum with stable `Display` strings — stable because
tests assert on them and they surface verbatim as the RFC 9457 problem `detail`
once mapped at the HTTP boundary (you wire that mapping in [CQRS](./09-cqrs.md)
and [Your First HTTP API](./06-first-http-api.md)).

Add `DomainError` and its impls to `src/domain.rs`:

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

What just happened:

- **`From<MoneyError> for DomainError` is the bridge.** It is what lets
  `withdraw` write `self.balance.subtract(amount)?` and have an arithmetic
  overdraw surface as `InsufficientFunds`. The value object reports the
  arithmetic fact (`Overdraw`); the aggregate translates it into domain language
  (`InsufficientFunds`). The `?` operator calls this `From` impl automatically.
- **The `Display` strings are the contract.** `"insufficient funds"`,
  `"amount must be positive"`, `"wallet {id} not found"`, `"owner is required"`
  — these exact strings become the RFC 9457 problem `detail` at the web
  boundary, and tests assert on them, so they are stable.
- **Hand-written `Display` and `Error`, again.** Like `MoneyError`, `DomainError`
  spells out its `Display` and `Error` impls instead of deriving with
  `thiserror`: no extra crate, one dependency.

> **Note** Lumen returns a typed `DomainError` rather than throwing.
> `InsufficientFunds` / `NonPositiveAmount` / `OwnerRequired` become 422 problems
> and `NotFound` becomes a 404, decided by a `match` at the web boundary — a
> returned value, checked by the compiler, with no exception-to-status table to
> keep in sync. You will write that `match` in [CQRS](./09-cqrs.md).

> **Tip** **Checkpoint.** `cargo build` now resolves `Wallet::open` /
> `deposit` / `withdraw` and their error type. The `raise` helper is still
> missing (next step), so the build is not green yet — but every domain rule is
> now expressed as code.

## Step 9 — Add the `raise` helper and the read-model view

Two pieces finish `src/domain.rs`. First, the private `raise` helper that the
command methods call — it serializes a `#[derive(DomainEvent)]` payload and
pushes it onto the embedded root. Second, the flat read-model view the aggregate
hands out.

> **Note** **Key term — read model / projection.** A *read model* (or
> *projection*) is a flat, query-optimized view of an aggregate, separate from
> the rich aggregate itself. The aggregate is the *write* model — rule-enforcing,
> event-raising; the read model is what queries return — serializable, no
> behavior. Keeping them separate is the heart of CQRS
> ([CQRS](./09-cqrs.md)). The Spring analog is a JPA read projection / DTO
> distinct from the managed entity.

Add the `view` method and `raise` helper to the `impl Wallet` block, then the
`WalletView` struct:

```rust,ignore
    /// The current read-model view of this aggregate.
    pub fn view(&self) -> WalletView {
        WalletView {
            id: self.root.id.clone(),
            owner: self.owner.clone(),
            balance: self.balance.cents_value(),
            version: self.root.version,
        }
    }

    /// Serialises a `#[derive(DomainEvent)]` payload and raises it onto the
    /// embedded root under `event_type`.
    fn raise<P: Serialize>(&mut self, event_type: &str, payload: &P) {
        let bytes = serde_json::to_vec(payload).expect("domain event payload serialises");
        self.root.raise(event_type, bytes);
    }
}

/// The read-model projection of a wallet — the wire shape served by
/// `GET /api/v1/wallets/:id` and stored in the read-model repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WalletView {
    pub id: String,
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied).
    pub version: i64,
}
```

What just happened:

- **`raise` is the one place events are serialized.** It takes the
  `EVENT_TYPE` discriminator and a serializable payload, encodes the payload to
  bytes with `serde_json::to_vec`, and calls `self.root.raise(event_type,
  bytes)` — the framework method on `AggregateRoot` that appends to the
  uncommitted-event buffer and bumps the version. Every command method routes
  through this helper, so the serialization lives in exactly one spot.
- **`view()` produces the read model on demand.** It copies `id`, `owner`,
  `balance` (as bare cents via `cents_value()`), and `version` (off
  `root.version`) into a flat `WalletView`. The aggregate never serializes
  *itself* — it hands out a view.
- **`WalletView` derives `Schema`.** That makes it appear in the auto-generated
  OpenAPI docs as a component schema, so `GET /api/v1/wallets/:id`'s response is
  documented with zero extra code (see [OpenAPI](./06a-openapi.md)).
- **`version` lets a client detect staleness.** Under the eventual consistency
  CQRS introduces, a client can compare versions to notice it read a stale
  projection.

Keeping `Wallet` (rich, rule-enforcing, event-raising) and `WalletView` (flat,
serializable, no behavior) as separate types is the same domain/persistence
separation [Persistence](./07-persistence.md) drew around the repository: the
aggregate never serializes itself, and the wire shape never carries an
invariant.

> **Tip** **Checkpoint.** `cargo build` is green. `src/money.rs` and
> `src/domain.rs` both compile, and `Wallet::open(...).view()` round-trips a
> wallet from a factory call to a serializable view — with every rule enforced in
> between.

## Step 10 — Prove the invariants with unit tests

Because the aggregate is a plain struct with plain methods, you can exercise
every rule with no database and no HTTP. These are the unit tests that ship in
`samples/lumen/src/domain.rs`. Add a `#[cfg(test)] mod tests` block:

```rust,ignore
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_validates_owner_and_balance() {
        assert_eq!(
            Wallet::open("w1", "  ", Money::cents(100)).unwrap_err(),
            DomainError::OwnerRequired
        );
        assert_eq!(
            Wallet::open("w1", "alice", Money::cents(-1)).unwrap_err(),
            DomainError::NonPositiveAmount
        );
        let w = Wallet::open("w1", "alice", Money::ZERO).unwrap();
        assert!(w.opened);
        assert_eq!(w.balance, Money::ZERO);
    }

    #[test]
    fn withdraw_rejects_overdraft() {
        let mut w = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
        assert_eq!(
            w.withdraw(Money::cents(101)).unwrap_err(),
            DomainError::InsufficientFunds
        );
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

    #[test]
    fn wallet_view_wire_shape() {
        let w = Wallet::open("wlt_1", "alice", Money::cents(250)).unwrap();
        let json = serde_json::to_string(&w.view()).unwrap();
        assert_eq!(
            json,
            r#"{"id":"wlt_1","owner":"alice","balance":250,"version":1}"#
        );
    }
}
```

What just happened:

- **`open_validates_owner_and_balance`** proves the birth invariants: a blank
  owner is `OwnerRequired`, a negative opening balance is `NonPositiveAmount`,
  and a *zero* opening balance succeeds (the wallet is `opened` with a `ZERO`
  balance).
- **`withdraw_rejects_overdraft`** is the load-bearing test. After a rejected
  withdrawal, the aggregate's uncommitted-event buffer (`w.root.uncommitted()`)
  still holds *exactly one* event — the `WalletOpened` from the factory. The
  overdraft never produced a `MoneyWithdrawn`, so nothing partial can ever be
  persisted. This is the consistency boundary made concrete.
- **`deposit_and_withdraw_update_balance_and_raise_events`** walks the happy
  path: a deposit lifts the balance, a withdrawal lowers it, and the arithmetic
  is exact (`100 + 50 - 30 = 120`).
- **`wallet_view_wire_shape`** pins the wire contract. A freshly opened wallet's
  `view()` serializes to exactly
  `{"id":"wlt_1","owner":"alice","balance":250,"version":1}` — confirming
  `Money`'s `#[serde(transparent)]` (the balance is the bare number `250`, not
  `{ "cents": 250 }`) and `WalletView`'s field order.

> **Tip** **Checkpoint.** Run `cargo test --lib`. All the domain tests pass — and
> they ran with no database, no HTTP server, and no framework runtime. That is
> the payoff of a domain core that is just structs and methods: the rules are
> testable in microseconds.

## Recap — Lumen's domain core

Lumen now has a domain core that owns its rules:

- **`src/money.rs`** — a `Money` value object: immutable, integer cents, private
  field, `#[serde(transparent)]` so it rides the wire as a bare number, closed
  under `add` / `subtract` / `require_positive`, with a hand-written
  `MoneyError` (no `thiserror`) and a `Display` that prints `"12.50"`.
- **`src/domain.rs`** — the `Wallet` aggregate carrying
  `#[derive(AggregateRoot)]` (which generates `AGGREGATE_TYPE` and the
  `aggregate()` / `aggregate_mut()` accessors over the embedded root). `open` /
  `deposit` / `withdraw` enforce the three invariants — owner required, amounts
  positive, no overdraft — *before* raising an event, so a rejected command
  leaves the event buffer untouched.
- **Three `#[derive(DomainEvent)]` payloads** (`WalletOpened`,
  `MoneyDeposited`, `MoneyWithdrawn`), each stamped with a stable `EVENT_TYPE`
  discriminator, raised through the single private `raise` helper.
- **The typed `DomainError` family** with stable `Display` strings that surface
  as RFC 9457 problem details, plus the `From<MoneyError>` bridge that turns an
  arithmetic overdraw into `InsufficientFunds`.
- **`WalletView`** — the flat read-model projection the aggregate hands out via
  `view()`, deriving `Schema` for the docs, kept a separate type from the
  rule-enforcing aggregate.

You also now know:

- The difference between a **value object** (no identity, immutable, compared by
  value — `Money`) and an **aggregate** (an identity and a consistency boundary —
  `Wallet`), and why each rule belongs where it does.
- That an aggregate makes its invariants *physically* unreachable by validating
  before it mutates and exposing no setters — the difference between a convention
  and a constraint.
- That `#[derive(AggregateRoot)]` and `#[derive(DomainEvent)]` supply the event
  buffer and the type discriminators, so the only event-sourcing code you write
  by hand is the rules.

The event payloads' full lifecycle — how they are persisted, and the
`rehydrate` / `apply` fold that reconstructs a wallet from its stream — gets its
treatment in [Event Sourcing](./11-event-sourcing.md). Here, the shape that
matters is the DDD one: a value object you cannot corrupt and an aggregate you
cannot bypass.

## Exercises

1. **Break an invariant, watch it hold.** In a `#[cfg(test)]` block, open a
   wallet with `Money::cents(100)` and call `withdraw(Money::cents(200))`. Assert
   the error is `DomainError::InsufficientFunds` *and* that
   `w.root.uncommitted().len()` is still `1` — proving the rejected command
   raised no event beyond the open.

2. **Add a `transfer` rule (domain only).** Write a free function
   `fn transfer(from: &mut Wallet, to: &mut Wallet, amount: Money) ->
   Result<(), DomainError>` that calls `from.withdraw(amount)?` then
   `to.deposit(amount)?`. Test that a transfer exceeding the source balance
   fails on the withdraw leg and leaves the *target* balance unchanged. (The
   real, persisted version becomes the saga in [Sagas](./12-sagas.md).)

3. **Confirm the wire shape.** Serialize a freshly opened wallet's `view()` with
   `serde_json::to_string` and assert it equals
   `{"id":"wlt_1","owner":"alice","balance":250,"version":1}` — verifying that
   `Money`'s `#[serde(transparent)]` and `WalletView`'s field order produce the
   contract the read model and clients share.

4. **Justify the hand-written error.** In two sentences, explain why
   `MoneyError` and `DomainError` implement `Display` / `Error` by hand instead
   of deriving with `thiserror`, and what it would cost the book's
   one-dependency promise to add the crate.

5. **Allow a zero deposit — then decide against it.** Change `deposit` to accept
   a zero amount and run the suite; note which test breaks and why
   `require_positive` rejecting zero is the right rule for a wallet command.
   Revert the change.

## Where to go next

- Separate the write path from the read path with the command/query bus in
  **[CQRS](./09-cqrs.md)** — where `Wallet`'s commands become bus-dispatched
  handlers and `DomainError` becomes the RFC 9457 problem mapping.
- Expose these rules over HTTP in
  **[Your First HTTP API](./06-first-http-api.md)**, where a returned
  `DomainError` renders as a 422 or 404 problem document.
- Persist the events and reconstruct a wallet from its stream in
  **[Event Sourcing](./11-event-sourcing.md)** — the full lifecycle of the
  payloads you raised here.
