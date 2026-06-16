# Event Sourcing

The [last chapter](./10-eda-messaging.md) left one question politely unasked.
Lumen's `Ledger` persists wallet events and a projection rebuilds the read model
by re-folding the stream — but *what stream?* So far the wallet's canonical state
has been implied. By the end of this chapter it is explicit and load-bearing: the
`Wallet` aggregate holds **no stored balance at all**. Its balance is a pure
function of an append-only stream of `WalletOpened`, `MoneyDeposited`, and
`MoneyWithdrawn` events, recomputed every time the aggregate is loaded.

That is **event sourcing**: instead of storing current state and discarding each
change, you store the *sequence of changes* and derive state by replaying them. A
financial ledger is the ideal domain for it — accountants have known for
centuries that a ledger's authority comes from its entries, not from the running
total at the foot of the column. The total is a *derived fact*; the entries are
the *source of truth*. By the end, an auditor asking "what was wallet `wlt_…`'s
balance after the third movement?" gets an answer Lumen can *prove* from the
stream, not merely report from a column.

This chapter is a guided build. We introduce each piece from first principles,
write it block by block against the real `firefly-eventsourcing` API, and stop at
checkpoints so you can confirm what you have before moving on. Nothing here is
hand-waved: every type, method, and derive matches the crate that ships in
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen).

By the end of this chapter you will:

- Explain the difference between **state storage** and **event storage**, and why
  the wallet's balance becomes a computation rather than a column.
- Define domain events with `#[derive(DomainEvent)]` and an event-sourced
  aggregate with `#[derive(AggregateRoot)]`, and know exactly what each derive
  generates.
- Implement the canonical command shape — validate, `raise`, then `apply` — and
  understand why the **same fold runs on both the write path and replay**.
- Persist and reload events through the `EventStore` port with **optimistic
  concurrency**, and handle a concurrency conflict correctly.
- Recognise the production-grade seams the crate provides — snapshots,
  projections, the global stream, the transactional outbox, upcasters, and
  multi-tenancy — and know when each one earns its keep.

## Concepts you will meet

Each idea below is reintroduced in context where it is first used; this is the
short version so the vocabulary is not new when you reach it.

> **Note** **Key term — event sourcing.** A persistence style where you store the
> ordered *sequence of changes* (events) to an entity rather than its current
> state, and recompute state by replaying that sequence. The Java/Spring analog
> is the `firefly-event-sourcing-spring-boot-starter` (or Axon Framework's
> event-sourced aggregates).

> **Note** **Key term — domain event.** An immutable record that *something
> happened* in the domain, named in the past tense (`MoneyDeposited`). In
> event sourcing the events are the system of record. This is distinct from the
> EDA `Event` envelope of the [last chapter](./10-eda-messaging.md), which is the
> *transport* for a fact; the `DomainEvent` here is the durable *record* of it.

> **Note** **Key term — aggregate.** A cluster of domain objects treated as a
> single consistency boundary, with one **aggregate root** as its entry point.
> Every command goes through the root, which enforces the aggregate's invariants.
> Lumen's aggregate is the `Wallet`; its root is the embedded framework
> `AggregateRoot`. This is the Domain-Driven Design "aggregate" Spring developers
> know from `@Entity` roots — but here it is reconstructed from events, not loaded
> from a row.

> **Note** **Key term — optimistic concurrency.** A way to detect concurrent
> writes without locking: each write declares the version it expected to find,
> and the store rejects it if another writer got there first. The Spring/JPA
> analog is `@Version` optimistic locking.

## Step 1 — Feel the shift: state storage vs event storage

Before writing a line, look at what Lumen's storage *holds* in each model. The
contrast is the whole motivation for this chapter.

In the **state-storage model** — the default everywhere else — the store keeps
only the wallet's current state:

| id | owner | balance | version |
|----|-------|---------|---------|
| wlt_a1 | alice | 120 | 3 |

Every deposit and withdrawal overwrites `balance`. The history is gone: you know
the wallet holds 120 cents now; you cannot know how it got there.

In the **event-storage model**, the store keeps the stream:

| aggregate_id | version | event_type | payload |
|--------------|---------|------------|---------|
| wlt_a1 | 1 | WalletOpened | `{"wallet_id":"wlt_a1","owner":"alice","opening_balance":100}` |
| wlt_a1 | 2 | MoneyDeposited | `{"wallet_id":"wlt_a1","amount":50}` |
| wlt_a1 | 3 | MoneyWithdrawn | `{"wallet_id":"wlt_a1","amount":30}` |

The current balance is still 120 cents — but now you can read every decision that
led to it, replay to any version, and audit the lot.

What just happened: the same final balance now has a *derivation*. The trade-off
is real and worth naming up front — reads cost a replay (mitigated by
**snapshots**, Step 8) and events are immutable (schema change handled by
**upcasters**, Step 11). Both have first-class support, and you will meet them in
turn.

> **Note** Event sourcing is *not* the same as the
> [previous chapter](./10-eda-messaging.md)'s EDA. There, the aggregate stored its
> state and *published* events as a side effect. Here the events *are* the state:
> there is no `balance` column to keep in sync — the balance is computed by
> folding the stream every time the aggregate loads.

> **Tip** **Checkpoint.** You can state, in one sentence, what each table loses or
> keeps: state storage keeps the answer and discards the working; event storage
> keeps the working and recomputes the answer. The rest of the chapter makes that
> recomputation concrete.

## Step 2 — The mental model: raise, append, fold

Everything below is three moves repeated. A command **raises** an event onto the
aggregate; the store **appends** the raised events durably under optimistic
concurrency; a later load **folds** the stream back into current state. Hold this
cycle in mind — every API in the chapter is one of these three moves.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 322" role="img"
     aria-label="Event sourcing: a command raises an event onto the aggregate, EventStore append persists the events to an append-only stream under optimistic concurrency, and a later load folds the stream back into the current state"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="150.0" y="24.0" text-anchor="middle" font-size="11.5" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">write path</text>
<rect x="50.0" y="38.5" width="200.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="36.0" width="200.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="150.0" y="59.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Command</text><text x="150.0" y="73.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Deposit { amount }</text><line x1="150.0" y1="88.0" x2="150.0" y2="102.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="150.0,110.0 145.5,102.0 154.5,102.0" fill="#b5531f"/><rect x="50.0" y="112.5" width="200.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="110.0" width="200.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="150.0" y="133.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">raise(event)</text><text x="150.0" y="147.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">→ uncommitted []</text><line x1="150.0" y1="162.0" x2="150.0" y2="176.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="150.0,184.0 145.5,176.0 154.5,176.0" fill="#b5531f"/><rect x="50.0" y="186.5" width="200.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="184.0" width="200.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="150.0" y="207.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">append(events)</text><text x="150.0" y="221.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">optimistic concurrency</text>
<text x="420.0" y="24.0" text-anchor="middle" font-size="11.5" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">event stream (append-only)</text>
<rect x="330.0" y="46.5" width="180.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="330.0" y="44.0" width="180.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="420.0" y="66.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">+100</text><text x="420.0" y="80.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">WalletOpened</text>
<line x1="420.0" y1="94.0" x2="420.0" y2="106.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="420.0,114.0 415.5,106.0 424.5,106.0" fill="#b5531f"/>
<rect x="330.0" y="116.5" width="180.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="330.0" y="114.0" width="180.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="420.0" y="136.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">+50</text><text x="420.0" y="150.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">MoneyDeposited</text>
<line x1="420.0" y1="164.0" x2="420.0" y2="176.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="420.0,184.0 415.5,176.0 424.5,176.0" fill="#b5531f"/>
<rect x="330.0" y="186.5" width="180.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="330.0" y="184.0" width="180.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="420.0" y="206.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">−30</text><text x="420.0" y="220.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">MoneyWithdrawn</text>
<line x1="250.0" y1="198.0" x2="324.6" y2="115.9" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="330.0,110.0 327.9,118.9 321.3,112.9" fill="#b5531f"/><text x="290.0" y="150.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">append</text>
<line x1="330.0" y1="244.0" x2="257.1" y2="282.3" stroke="#1f8a4c" stroke-width="3.0" stroke-linecap="round"/><polygon points="250.0,286.0 255.0,278.3 259.2,286.3" fill="#1f8a4c"/><text x="290.0" y="279.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">fold / replay</text>
<rect x="50.0" y="266.5" width="200.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="264.0" width="200.0" height="46.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="150.0" y="284.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">current state</text><text x="150.0" y="298.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">balance = 120</text>
</svg>
<figcaption>Three moves. A command <code>raise</code>s an event onto the aggregate; <code>EventStore::append</code> persists the uncommitted events under optimistic concurrency; a later load <code>fold</code>s the whole append-only stream back into the current state — the events are the source of truth, the state is derived.</figcaption>
</figure>

The framework piece that powers all three moves is `firefly-eventsourcing`,
re-exported through the facade as `firefly::eventsourcing`.

> **Note** **Key term — `firefly-eventsourcing`.** The framework's event-sourcing
> crate. It provides the `AggregateRoot` (uncommitted-event buffer + version), the
> `EventStore` port (append/load with optimistic concurrency), snapshots,
> projections, a global cross-aggregate stream, a transactional outbox, upcasters,
> and multi-tenancy. You depend on none of it directly — it arrives through the
> single `firefly` facade, and the two derives (`DomainEvent`, `AggregateRoot`)
> come in through `firefly::prelude`.

## Step 3 — Define the Wallet's domain events

The action: declare the three events the wallet can produce. In Lumen each is a
plain payload struct carrying `#[derive(DomainEvent)]`. They live in
`src/domain.rs`.

```rust
use firefly::eventsourcing::{AggregateRoot, DomainEvent};
use firefly::prelude::*;
use serde::{Deserialize, Serialize};

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
    pub amount: i64,
}

/// Payload of the event raised when money is debited from a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyWithdrawn {
    pub wallet_id: String,
    pub amount: i64,
}
```

What just happened, block by block:

- The two **types** `AggregateRoot` and `DomainEvent` come from
  `firefly::eventsourcing`. The two **derive macros** of the same name come from
  `firefly::prelude::*` — the glob that re-exports every framework macro, so a
  service depends on one crate yet still writes `#[derive(DomainEvent)]`.
- `Serialize`/`Deserialize` make each payload JSON-encodable; the derive needs
  `Serialize` because it JSON-encodes the payload into the stored event.
- Each event is named in the **past tense** and carries only the data the fact
  needs. `opening_balance` and `amount` are in minor units (cents) — Lumen never
  stores floating-point money.

Now the important part — what `#[derive(DomainEvent)]` generates. For each struct
it produces:

- a `pub const EVENT_TYPE: &'static str` equal to the struct name
  (`"WalletOpened"`, `"MoneyDeposited"`, `"MoneyWithdrawn"`) — the routing
  discriminator;
- an `event_type()` accessor returning that const;
- a `to_domain_event(aggregate_id, aggregate_type, version)` method that
  JSON-encodes the payload into a framework `DomainEvent`.

That generated `EVENT_TYPE` const is the *only* thing the aggregate and its fold
reference, so the event type is never a bare string literal at the call sites —
and a rename of the struct flows through automatically.

> **Note** **Key term — `DomainEvent` (the wire type).** Beside the derive there
> is a concrete `firefly::eventsourcing::DomainEvent` struct: the wire shape of
> every persisted event, with an `aggregate_id`, `aggregate_type`, 1-based
> `version`, `event_type`, `time`, a base64 `payload`, optional `metadata`, and an
> optional `tenant_id`. Its JSON is a stable, versioned, language-neutral contract
> — byte-compatible with the Java, .NET, Go, and Python ports, so any service that
> honours it interoperates regardless of language.

> **Tip** **Checkpoint.** `cargo build` compiles the three structs. In a quick
> test you can assert `WalletOpened::EVENT_TYPE == "WalletOpened"` and round-trip
> a payload through `serde_json::to_vec` / `from_slice`. The events exist; nothing
> raises them yet.

## Step 4 — Define the Wallet aggregate

The action: declare the aggregate that produces those events. The `Wallet`
carries `#[derive(AggregateRoot)]`, which finds the embedded framework
`AggregateRoot` field and wires up the type discriminator and accessors. Crucially,
the projected state (`owner`, `balance`, `opened`) is **not stored** — it is folded
from the stream.

```rust
use firefly::eventsourcing::{AggregateRoot, DomainEvent};

use crate::money::Money;

/// The aggregate-type discriminator stamped onto every event a wallet raises.
pub const AGGREGATE_TYPE: &str = "Wallet";

#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    /// The framework aggregate root — uncommitted-event buffer + version.
    pub root: AggregateRoot,
    pub owner: String,
    /// Folded from the stream; never stored.
    pub balance: Money,
    /// Whether the wallet has been opened (an empty stream is "absent").
    pub opened: bool,
}
```

What just happened:

- The embedded `root: AggregateRoot` field is the framework's bookkeeping — it
  holds the aggregate id, the current version, and the buffer of *uncommitted*
  events that commands raise but the store has not yet persisted. Rust composes
  this field rather than subclassing a base class.
- `#[derive(AggregateRoot)]` locates that `root` field (the default field name;
  override with `#[firefly(field = "...")]`) and generates a
  `Wallet::AGGREGATE_TYPE` const plus `aggregate()` / `aggregate_mut()` accessors
  onto the embedded root. `#[firefly(aggregate_type = "Wallet")]` sets the
  discriminator explicitly (it would default to the struct name anyway).
- `owner`, `balance`, and `opened` are **projected fields**: they exist only in
  memory and are reconstructed by folding the stream. `Money` is Lumen's
  cents-based value object from `src/money.rs`.

> **Note** **Key term — uncommitted events.** Events a command has `raise`d onto
> the aggregate root but the store has not yet persisted. They live in the root's
> buffer until you `take_uncommitted()` them and hand them to `EventStore::append`.
> Think of them as the aggregate's pending write.

> **Tip** **Checkpoint.** `cargo build` succeeds and `Wallet::AGGREGATE_TYPE`
> evaluates to `"Wallet"`. The aggregate is declared but has no behaviour yet —
> Step 5 adds the commands.

## Step 5 — Write a command: validate, raise, apply

The action: give the wallet behaviour. Every command follows the canonical
event-sourcing shape — validate the invariant, `raise` the matching event onto the
embedded root, then apply it to in-memory state. Here is `deposit`, plus the small
private helper that serialises a payload and raises it.

```rust,ignore
/// Credits `amount` to the wallet, raising a `MoneyDeposited` event.
pub fn deposit(&mut self, amount: Money) -> Result<(), DomainError> {
    self.require_opened()?;
    let amount = amount.require_positive()?;
    self.raise(
        MoneyDeposited::EVENT_TYPE,
        &MoneyDeposited {
            wallet_id: self.root.id.clone(),
            amount: amount.cents_value(),
        },
    );
    self.balance = self.balance.add(amount);
    Ok(())
}

/// Serialises a `#[derive(DomainEvent)]` payload and raises it onto the embedded
/// root under `event_type` — the discriminator from the generated `EVENT_TYPE`.
fn raise<P: Serialize>(&mut self, event_type: &str, payload: &P) {
    let bytes = serde_json::to_vec(payload).expect("domain event payload serialises");
    self.root.raise(event_type, bytes);
}
```

What just happened, in order:

1. `require_opened()?` enforces the invariant: you cannot deposit into a wallet
   that was never opened. A failed check returns `DomainError::NotFound` and
   raises **no** event.
2. `amount.require_positive()?` rejects a non-positive deposit before any event is
   recorded.
3. `self.raise(MoneyDeposited::EVENT_TYPE, …)` records the fact. Note the event
   type is the generated const, never a string literal. The private `raise`
   helper JSON-encodes the payload and calls `self.root.raise(event_type, bytes)`.
4. `self.balance = self.balance.add(amount)` updates the in-memory projection.

The framework's `AggregateRoot::raise` does two things: it pushes the event onto
the uncommitted buffer (so the ledger can persist it later) and bumps the version
by one. That version bump is what later powers optimistic concurrency.

`withdraw` is the same shape with one extra guard worth seeing, because the
transfer saga in [Sagas, Workflows & TCC](./12-sagas.md) depends on it:

```rust,ignore
/// Debits `amount` from the wallet, raising a `MoneyWithdrawn` event.
pub fn withdraw(&mut self, amount: Money) -> Result<(), DomainError> {
    self.require_opened()?;
    let amount = amount.require_positive()?;
    let remaining = self.balance.subtract(amount)?; // Overdraw → InsufficientFunds
    self.raise(
        MoneyWithdrawn::EVENT_TYPE,
        &MoneyWithdrawn {
            wallet_id: self.root.id.clone(),
            amount: amount.cents_value(),
        },
    );
    self.balance = remaining;
    Ok(())
}
```

Why it matters: `Money::subtract` is computed *first* and rejects an overdraw with
`MoneyError::Overdraw` (mapped to `DomainError::InsufficientFunds`) **before**
`raise` is ever reached. A failed withdrawal therefore raises no event at all,
leaving the stream clean. That overdraft guard is the failure trigger the transfer
saga relies on.

> **Tip** **Checkpoint.** With `open`, `deposit`, and `withdraw` written, a unit
> test can open a wallet, deposit 50, withdraw 30, then call
> `wallet.take_uncommitted()` and assert it holds exactly three events in order.
> Lumen's own `domain.rs` tests do precisely this.

## Step 6 — Rehydrate: fold the stream back into state

The action: rebuild a wallet from its events. **Rehydration** is the load path —
it replays the full ordered stream through the same `apply` the commands use. An
empty stream yields an *unopened* wallet, which is how the ledger distinguishes
"absent" from "exists".

> **Note** **Key term — rehydration.** Reconstructing an aggregate's current state
> by folding its event stream from the beginning. The Spring/Axon analog is an
> event-sourced repository's `load`, which replays the aggregate's events into a
> fresh instance.

```rust,ignore
/// Rebuilds a wallet by folding `events` (its full ordered stream).
pub fn rehydrate(id: &str, events: &[DomainEvent]) -> Self {
    let mut wallet = Wallet {
        root: AggregateRoot::new(id, AGGREGATE_TYPE),
        owner: String::new(),
        balance: Money::ZERO,
        opened: false,
    };
    for event in events {
        wallet.apply(event);
        // Keep the root version in lock-step with the stream head so a
        // subsequent command appends at the right expected version.
        wallet.root.version = event.version;
    }
    wallet
}

/// Folds one persisted event into the projected state.
fn apply(&mut self, event: &DomainEvent) {
    match event.event_type.as_str() {
        WalletOpened::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<WalletOpened>(&event.payload) {
                self.owner = p.owner;
                self.balance = Money::cents(p.opening_balance);
                self.opened = true;
            }
        }
        MoneyDeposited::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<MoneyDeposited>(&event.payload) {
                self.balance = self.balance.add(Money::cents(p.amount));
            }
        }
        MoneyWithdrawn::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<MoneyWithdrawn>(&event.payload) {
                self.balance = Money::cents(self.balance.cents_value() - p.amount);
            }
        }
        _ => {}
    }
}
```

What just happened:

- `rehydrate` starts from a blank wallet (`opened: false`, zero balance) and folds
  each event through `apply`, keeping `root.version` in step with the stream head.
  After the fold, `root.version` equals the version of the last event — which is
  exactly the token the next command will append against.
- `apply` matches on `event.event_type` against the generated `EVENT_TYPE`
  constants — the same constants the commands raise under — so the write fold and
  the replay fold can never disagree about an event's name.

One subtlety worth pausing on. `apply` folds `MoneyWithdrawn` with a *raw*
subtraction (`self.balance.cents_value() - p.amount`) rather than the
overdraft-guarded `Money::subtract` the `withdraw` *command* uses. That asymmetry
is deliberate: **replay never re-validates**. The guard already ran at write time,
and a failed withdrawal raised no event, so every event in the stream is a fact
that already passed its invariant. Replay simply applies it.

> **Design note.** This is the correctness guarantee of event sourcing made
> concrete. A command `raise`s an event and `apply` mutates the projected fields;
> a load replays the *same* `apply` to rebuild state. Lumen registers no handler
> table — it `match`es on the generated `EVENT_TYPE` const, the Rust-idiomatic way
> to keep the write fold and the replay fold from ever disagreeing about an
> event's name.

> **Tip** **Checkpoint.** This is the law to prove: open + deposit + withdraw on a
> *writer* wallet, take its uncommitted stream, then `Wallet::rehydrate` a fresh
> wallet from that stream and assert the rebuilt balance, owner, and version all
> match — state recomputed from events, never stored. Lumen's `rehydrate_folds_the_full_stream`
> test does exactly this.

## Step 7 — Persist and reload through the `EventStore`

The action: make the events durable. The framework `AggregateRoot` accumulates
`DomainEvent`s as you `raise` them; you `take_uncommitted` them and `append` them
to an `EventStore`. The store enforces optimistic concurrency — you pass the
version you loaded, and a concurrent writer's append fails.

Here is the move in isolation, against the in-process store:

```rust
use firefly::eventsourcing::{AggregateRoot, EventStore, MemoryEventStore};

#[tokio::main]
async fn main() {
    let store = MemoryEventStore::new();

    let mut user = AggregateRoot::new("u1", "User");
    user.raise("UserCreated", br#"{"name":"alice"}"#);
    user.raise("UserRenamed", br#"{"name":"bob"}"#);

    let events = user.take_uncommitted();
    // expected_version 0 -> this is a brand-new aggregate.
    if let Err(err) = store.append(&user.id, 0, events).await {
        eprintln!("append failed (raced): {err}");
    }

    assert_eq!(store.load("u1").await.unwrap().len(), 2);
}
```

What just happened: two `raise` calls buffer two events and bump the root to
version 2. `take_uncommitted()` drains the buffer (a Rust-idiomatic fusion of
"return the events" + "clear them"). `append(&id, 0, events)` persists them, where
`0` is the **expected version** — the head we expected to find before writing.
Because the aggregate is brand new, that head is `0`; the append succeeds. Reading
the stream back returns both events in order.

The `EventStore` port — the contract every store implements:

```rust,ignore
#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, aggregate_id: &str, expected_version: i64,
                    events: Vec<DomainEvent>) -> Result<(), EventSourcingError>;
    async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError>;
    async fn load_after(&self, aggregate_id: &str, since_version: i64)
        -> Result<Vec<DomainEvent>, EventSourcingError>;
    async fn stream_all(&self, after_event_id: Option<&str>, limit: usize, tenant: Option<&str>)
        -> Result<Vec<StreamedEvent>, EventSourcingError>;
}
```

> **Note** **Key term — `EventStore` port / `MemoryEventStore` adapter.** The
> `EventStore` trait is the persistence boundary — a *port* in the hexagonal
> sense. `MemoryEventStore` is the in-process *adapter* Lumen runs on by default,
> ideal for development and tests. `SqlEventStore::new(db)` is the production
> adapter over the `firefly-transactional` `Database` port. Swapping them is a
> one-line change to the `event_store` `#[bean]` in `LumenBeans` — exactly like
> swapping the broker in the [last chapter](./10-eda-messaging.md).

That bean is the only place the choice lives:

```rust,ignore
#[bean]
impl LumenBeans {
    /// The in-memory event store (`@Bean`).
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }
    // ...
}
```

> **Tip** **Checkpoint.** Run the example above (or the equivalent `#[tokio::test]`).
> `store.load("u1")` returns a `Vec` of length 2. If you instead call
> `store.append(&user.id, 5, events)` for a fresh aggregate, you get
> `Err(EventSourcingError::Concurrency)` — proof the expected-version check is live.

## Step 8 — Wire it into the Ledger and handle concurrency

The action: tie persistence to the domain in one application service. Lumen's
`Ledger` (introduced in the [last chapter](./10-eda-messaging.md)) owns the store
and the broker. Every command rehydrates, runs the domain method, and commits with
optimistic concurrency. Here is `deposit` and the load path:

```rust,ignore
/// Credits `amount` to `wallet_id`, persisting + publishing `MoneyDeposited`.
pub async fn deposit(&self, wallet_id: &str, amount: Money) -> Result<WalletView, DomainError> {
    let mut wallet = self.load(wallet_id).await?;
    let expected = wallet.root.version;
    wallet.deposit(amount)?;
    self.commit(&mut wallet, expected).await?;
    Ok(wallet.view())
}

/// Rehydrates the aggregate from its persisted stream.
async fn load(&self, wallet_id: &str) -> Result<Wallet, DomainError> {
    let events = self.load_events(wallet_id).await?;
    Ok(Wallet::rehydrate(wallet_id, &events))
}

/// Loads the full event stream, mapping an absent aggregate to a domain 404.
pub async fn load_events(&self, wallet_id: &str) -> Result<Vec<DomainEvent>, DomainError> {
    match self.store.load(wallet_id).await {
        Ok(events) => Ok(events),
        Err(EventSourcingError::AggregateNotFound) => {
            Err(DomainError::NotFound(wallet_id.to_string()))
        }
        Err(e) => Err(DomainError::NotFound(format!("{wallet_id}: {e}"))),
    }
}
```

What just happened: `deposit` loads the wallet (rehydrating from its stream),
captures `wallet.root.version` as `expected`, runs the domain command, then
commits at `expected`. The version the wallet rehydrated to **is** the token the
append must match. `commit` (shown in full in the
[last chapter](./10-eda-messaging.md)) appends at `expected`, then publishes each
appended event to the broker so the projection can react. The two chapters meet
here: this one supplies the durable, replayable store; that one carries each
appended event onto the wire.

Now the concurrency case, because in a real system two writers race. Suppose a
deposit from the app and a fee withdrawal from a job both load wallet `wlt_a1` at
version 3, each apply a change, and each try to append at `expected_version = 3`.
The first append wins and the stream advances to 4; the second now mismatches, and
the store returns `EventSourcingError::Concurrency`. Lumen maps that to a
`DomainError::NotFound` carrying a "concurrent modification" detail so the caller
retries from a fresh load. You never manage version numbers by hand — the version
the wallet rehydrated to is the token, and the store enforces it.

> **Note** `append(id, expected_version, events)` enforces optimistic concurrency:
> the rehydrated version is the token, and a stale append fails with
> `EventSourcingError::Concurrency`. Catch it and retry the load-mutate-save cycle
> (or surface a 409) — never swallow it, or you risk losing a write.

> **Tip** **Checkpoint.** Append the open event for a wallet at
> `expected_version = 0`. Then, *without reloading*, raise a second event and
> append it *also* at `expected_version = 0`. The second append returns
> `EventSourcingError::Concurrency`. A fresh load (which advances `expected` to 1)
> would have succeeded — that is the whole mechanism in four lines.

## Step 9 — The thinner path: typed aggregates and the repository

Lumen folds the stream by hand in `Wallet::apply` because it teaches the mechanic
clearly. For larger aggregates the framework offers a thinner path: implement
`EventSourcedAggregate` — a typed `apply_event` plus optional snapshot
serialisation — and let `EventSourcedRepository` tie `load` (snapshot + replay) and
`save` (append + snapshot policy) together.

```rust,ignore
use firefly_eventsourcing::{
    AggregateRoot, DomainEvent, EventSourcedAggregate, EventSourcedRepository,
    EventSourcingError, MemoryEventStore,
};
use std::sync::Arc;

#[derive(Default)]
struct Wallet { root: AggregateRoot, balance: i64 }

impl EventSourcedAggregate for Wallet {
    const AGGREGATE_TYPE: &'static str = "Wallet";
    fn root(&self) -> &AggregateRoot { &self.root }
    fn root_mut(&mut self) -> &mut AggregateRoot { &mut self.root }
    fn apply_event(&mut self, event: &DomainEvent) -> Result<(), EventSourcingError> {
        if event.event_type == "Credited" {
            let amount: i64 = serde_json::from_slice(&event.payload)
                .map_err(|e| EventSourcingError::Projection(e.to_string()))?;
            self.balance += amount;
        }
        Ok(())
    }
}

# async fn ex() -> Result<(), EventSourcingError> {
let repo = EventSourcedRepository::<Wallet>::new(Arc::new(MemoryEventStore::new()));

let mut w = Wallet::default();
w.root_mut().raise("Credited", b"500");
repo.save(&mut w).await?;                     // append uncommitted

let reloaded = repo.load(&w.root.id).await?;  // snapshot + replay
assert!(reloaded.is_some());
# Ok(())
# }
```

What just happened: `EventSourcedAggregate` is the trait contract — it exposes the
embedded root via `root()` / `root_mut()` and the read-side fold via `apply_event`.
The repository then orchestrates the glue every event-sourced service otherwise
hand-writes: `save` computes the expected version from the uncommitted batch and
appends with optimistic concurrency; `load` returns `Ok(Some(_))` when the
aggregate has events and `Ok(None)` when it was never persisted. An event with no
handler should return `EventSourcingError::Projection` so reconstruction fails
loudly rather than silently corrupting state.

`EventSourcedRepository::with_snapshots(store, snapshots, interval)` enables
periodic state captures so rehydration does not replay the entire history — which
is the next step.

> **Tip** **Checkpoint.** You can articulate when each path is right: hand-fold
> (`Wallet::apply`) when the aggregate is small and you want the mechanic in view;
> `EventSourcedRepository` when you want load/save/snapshot orchestration handled
> for you. Both end at the same `EventStore`.

## Step 10 — Snapshots: bounding replay cost

Event sourcing trades write simplicity for read cost: a wallet with 10,000
movements replays 10,000 events every load. **Snapshots** cut that down.

> **Note** **Key term — snapshot.** A serialised checkpoint of an aggregate's
> state at a particular version. On load, the repository deserialises the latest
> snapshot and replays only the events *after* it — turning a 10,000-event replay
> into 1,000 if the snapshot sits at version 9,000. The Axon analog is its snapshot
> trigger.

Lumen's wallets are short-lived enough that the in-memory store's full replay is
fine, so the sample does not wire snapshots — but the seam is one constructor call:

```rust,ignore
use firefly_eventsourcing::{EventSourcedRepository, MemorySnapshotStore};

// Checkpoint each time a wallet's stream crosses a 100-event boundary.
let repo = EventSourcedRepository::<Wallet>::with_snapshots(
    store,
    Arc::new(MemorySnapshotStore::new()),
    100,
);
```

What just happened: `with_snapshots(store, snapshots, interval)` checkpoints
aggregate state every time a stream *crosses* an interval boundary. The trigger is
a crossing, not exact divisibility, so a batch that straddles the threshold
(version 95 → 105) still snapshots. On load, the repository restores the latest
snapshot and replays only the events after it.

> **Design note.** Snapshots are an optimisation, never a correctness requirement.
> Remove them and the system is slower but still correct — the events remain the
> source of truth, and the snapshot is just a cached fold of the prefix.

## Step 11 — Projections, the global stream, and the outbox

These three seams are how event sourcing feeds the rest of a system. You will not
wire all of them into the teaching baseline, but knowing the shape of each is part
of understanding the model.

### Projections — building read models from history

> **Note** **Key term — projection.** A read-side handler that consumes events to
> build a query-optimised read model. It must be **idempotent**, because events
> may be replayed during recovery. The Spring analog is a query-side
> `@EventListener` that updates a read table.

A `Projection` is registered on a `ProjectionRunner`, which can replay an
aggregate's events through it. This is the event-*store* sibling of the
[last chapter](./10-eda-messaging.md)'s event-*bus* listener: Lumen's live
`WalletProjection` reacts to events as they are published, whereas a
`ProjectionRunner` can replay history from the beginning to rebuild a read model
from scratch.

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{FunctionProjection, ProjectionRunner};

let runner = ProjectionRunner::new();
runner.register(Arc::new(FunctionProjection::new("balances", |event| async move {
    // update a read-model row from the event ...
    Ok(())
})));

runner.replay(&store, "wlt_a1").await?;  // replay one aggregate's stream
```

This rebuildability is unique to event sourcing. If Lumen's read model is ever
lost or its schema changes, you stop the projector, clear the read model, and
replay every stream — the history is right there in the store. A state-storage
model cannot do this; it discarded the history at write time.

### The global stream — read models across aggregates

`EventStore::stream_all` exposes the global, cross-aggregate, ordered event stream
with a resumable cursor — the engine for read models that span many aggregates
(think "all movements across all wallets, in order"). The runner consumes it in
batches, at-least-once and in-order:

```rust,ignore
// Drive one batch; returns the next cursor + any per-event error.
let (next_cursor, err) = runner
    .drive_once(&store, None, 100, None)
    .await?;

// Or replay the whole global stream from a start cursor.
let cursor = runner.replay_all(&store, None, 100, None).await?;
```

What just happened: `drive_once` applies one page and returns the cursor to resume
from, advancing it only past *successfully* applied events — so a failed event is
retried on the next call rather than skipped. `replay_all` drains the entire global
stream from a start cursor, paging `batch_size` at a time.

### The transactional outbox — closing the append-then-publish gap

The [last chapter](./10-eda-messaging.md) noted a gap in `Ledger::commit`: it
appends, then publishes, and a crash *between* the two persists the fact but drops
the broadcast. `TransactionalOutbox` closes that gap.

> **Note** **Key term — transactional outbox.** A pattern where a writer
> *enqueues* an event durably (ideally in the same store transaction as the
> append) instead of publishing it directly, and a background relay forwards each
> pending record to a broker, retrying on failure. Recording the event durably
> *before* dispatching it is what guarantees at-least-once delivery across
> crashes. This is the same outbox pattern Spring teams implement around their
> message broker.

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EdaSink, TransactionalOutbox};

let outbox = TransactionalOutbox::new(Arc::new(EdaSink::new(
    broker,           // the Arc<dyn firefly_eda::Publisher>
    "wallet.events",  // destination topic
    "lumen",          // logical source stamped onto every Event::source
)))
.with_max_attempts(5);

outbox.enqueue(some_event).await;       // a writer enqueues
outbox.start().await;                   // background relay forwards + retries
// ... later
let dead = outbox.dead_letters().await; // exhausted records, for inspection
outbox.stop().await;
```

What just happened: a writer `enqueue`s a `DomainEvent`; the relay (started with
`start()`) polls and forwards each pending record to an `OutboxSink`, retrying up
to `max_attempts`. The default `EdaSink` bridges each `DomainEvent` to a
`firefly_eda::Event` and publishes it — durable this time. Records that exhaust
`max_attempts` become **dead letters**: excluded from the publish loop and surfaced
via `dead_letters()` for inspection or manual retry. This is the production upgrade
path — and exactly why the projection was built to be **idempotent** in the last
chapter: at-least-once delivery means an event can arrive twice.

## Step 12 — Schema evolution and multi-tenancy

Two more seams round out the model. Both operate on the read path, so the stored
history stays immutable.

### Upcasters — migrating old events on read

> **Note** **Key term — upcaster.** A transform applied to a stored event when it
> is *read*, migrating it from an old schema to the current one. Consumers always
> observe current-schema events; the stored history is never rewritten. This is
> the event-sourcing answer to schema migration.

Suppose Lumen later needs a `reference` field on every deposit for reconciliation:
new events carry it, old `MoneyDeposited` events do not, and an upcaster fills the
gap on load:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EventUpcaster, MemoryEventStore};

let store = MemoryEventStore::with_upcasters(vec![Arc::new(MyUpcaster)]);
// every event returned by load / load_after passes through applicable upcasters
```

An `EventUpcaster` implements `applies_to(&event) -> bool` and
`upcast(event) -> DomainEvent`. Old data becomes readable without a migration; new
data is written in the current schema; the events themselves stay immutable. You
never rewrite history.

### Multi-tenancy — one store, many tenants

An optional `DomainEvent::tenant_id` (stamped from `AggregateRoot::with_tenant`,
persisted and filterable, omitted from JSON when `None`) is threaded through
`append` / `load` / `stream_all`. One store serves many tenants with per-tenant
isolation on the global stream — the route a multi-bank Lumen deployment would take
to keep each tenant's wallet streams separate. Because the field is omitted from
JSON when `None`, a single-tenant Lumen serialises byte-for-byte identically to the
cross-language wire format.

> **Tip** **Checkpoint.** You can name, for each seam, what it costs you if you
> *don't* use it: no snapshots → slower loads; no outbox → a crash can drop a
> publish; no upcaster → old events become unreadable after a schema change; no
> tenant id → you need one store per tenant. None of them changes the source of
> truth — they are all read-path or delivery concerns layered over the same
> immutable stream.

## Recap — what changed in Lumen

The wallet's balance is no longer a stored value — it is a *computation* over an
immutable stream, and the stream is the system of record.

| Piece | Role |
|-------|------|
| `#[derive(DomainEvent)]` | Generates `EVENT_TYPE` + `event_type()` + `to_domain_event(...)` for each payload struct |
| `#[derive(AggregateRoot)]` | Generates `AGGREGATE_TYPE` + `aggregate()` / `aggregate_mut()` over the embedded `root` |
| `Wallet` command (`deposit` / `withdraw`) | Validate the invariant, `raise` the event, apply to state |
| `Wallet::apply` / `rehydrate` | The same fold runs on write and on replay — an empty stream is "unopened" |
| `EventStore` / `MemoryEventStore` | The append-only log; `SqlEventStore` for production |
| `append(id, expected_version, …)` | Optimistic concurrency — the rehydrated version is the token |
| `EventSourcedRepository` | Ties load (snapshot + replay) and save (append + snapshot policy) together |
| `ProjectionRunner` | Rebuilds read models from history (the store-side sibling of the EDA listener) |
| `TransactionalOutbox` | Closes the append-then-publish gap with at-least-once relay |
| `EventUpcaster` / `tenant_id` | Schema evolution on read; per-tenant isolation across one store |

Three ideas carry forward:

- **The events are the truth.** There is no balance column to drift; the balance
  is folded from the stream on every load.
- **Write and replay share one fold.** `apply` runs the same way whether a command
  just raised the event or a load is rebuilding from history — and replay never
  re-validates, because every stored event already passed its invariant. That
  symmetry is the correctness guarantee.
- **Depend on the `EventStore` port.** The in-memory store becomes SQL with a
  one-line bean swap, just as the broker became Kafka — the domain never changes.

When a business process spans multiple aggregates and needs compensation — moving
money from one wallet to another, atomically — folding a single stream is no longer
enough. That is the next chapter.

## Exercises

1. **Replay to a point in time.** Open a wallet and make three deposits. Load the
   raw stream with `ledger.load_events(&id)`, take only the events with
   `version <= 2`, and `Wallet::rehydrate` a fresh wallet from that slice. Assert
   the balance equals opening + first deposit only — the "time-travel query" a
   state-storage model cannot answer.

2. **Prove the overdraft guard raises no event.** Open a wallet with 100 cents,
   attempt to `withdraw` 101, and assert it errors with
   `DomainError::InsufficientFunds`. Then call `wallet.root.uncommitted()` and
   assert the buffer still holds exactly one event (the `WalletOpened`) — the
   failed command left the stream clean.

3. **Force an optimistic-concurrency conflict.** Append the open event for a
   wallet at `expected_version = 0`. Then, without reloading, raise a second event
   and append it *also* at `expected_version = 0`. Assert the second append
   returns `EventSourcingError::Concurrency`, and explain why a fresh load (which
   advances `expected` to 1) would have succeeded.

4. **Add a `ProjectionRunner` rebuild.** Register a `FunctionProjection` that
   tallies the count of `MoneyDeposited` events per wallet into an in-memory map,
   `replay` one wallet's stream through it, and assert the count. Then clear the
   map and replay again — confirming the read model is rebuildable from the store
   alone, with no live event traffic.

5. **Swap the store (on paper).** Read the `event_store` `#[bean]` in
   `LumenBeans`, then write the one-line change that would return a
   `SqlEventStore::new(db)` instead of a `MemoryEventStore::new()`. Note that no
   command, no `apply`, and no `rehydrate` would change — only the bean. That is
   the payoff of depending on the `EventStore` port.

## Where to go next

- Coordinate a process across **two** wallets — debit one, credit the other, and
  compensate when the credit fails — in
  **[Sagas, Workflows & TCC](./12-sagas.md)**. The transfer saga is built directly
  on the overdraft guard and the optimistic-concurrency token from this chapter.
- Revisit how each appended event reaches the projection on the wire in
  **[Event-Driven Architecture & Messaging](./10-eda-messaging.md)** — the
  transport half of the story this chapter completed.
