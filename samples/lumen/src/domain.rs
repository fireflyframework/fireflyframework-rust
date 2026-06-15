// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The **Wallet aggregate** — Lumen's event-sourced domain core (book
//! chapters 6 "DDD" and 9 "Event Sourcing").
//!
//! A [`Wallet`] is reconstructed by folding the [`firefly::eventsourcing`]
//! `DomainEvent` stream its command methods produce. Each command
//! ([`Wallet::open`], [`Wallet::deposit`], [`Wallet::withdraw`]) validates the
//! invariant, `raise`s the matching event onto the embedded
//! [`AggregateRoot`], and applies it to in-memory state — the canonical
//! event-sourcing shape:
//!
//! ```text
//!   open(owner, opening)      ──► WalletOpened   ──► balance = opening
//!   deposit(amount)           ──► MoneyDeposited ──► balance += amount
//!   withdraw(amount)          ──► MoneyWithdrawn ──► balance -= amount  (if balance ≥ amount)
//! ```
//!
//! The three event payloads carry `#[derive(DomainEvent)]`, which stamps each
//! with a stable `EVENT_TYPE` discriminator (its struct name) and a
//! `to_domain_event(...)` conversion onto the framework wire event — the only
//! event-sourcing wiring Lumen writes by hand is the `apply` fold.

use firefly::eventsourcing::{AggregateRoot, DomainEvent};
use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::money::{Money, MoneyError};

/// The aggregate-type discriminator stamped onto every [`DomainEvent`] a
/// [`Wallet`] raises. `#[derive(AggregateRoot)]` also exposes it as
/// `Wallet::AGGREGATE_TYPE`.
pub const AGGREGATE_TYPE: &str = "Wallet";

/// The typed domain-error family. The `Display` strings are stable so tests
/// can assert on them and they surface verbatim as the RFC 9457 problem
/// `detail` once mapped at the HTTP boundary (see [`crate::web`]).
///
/// `Display` + `std::error::Error` are hand-written (no `thiserror`), keeping
/// Lumen's one-Firefly-dependency promise.
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

// ---------------------------------------------------------------------------
// Domain-event payloads — `#[derive(DomainEvent)]` stamps the discriminator.
// ---------------------------------------------------------------------------

/// Payload of the event raised when a wallet is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct WalletOpened {
    /// The new wallet's id.
    pub wallet_id: String,
    /// The owner's display name.
    pub owner: String,
    /// The opening balance, in minor units (cents).
    pub opening_balance: i64,
}

/// Payload of the event raised when money is credited to a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyDeposited {
    /// The credited wallet id.
    pub wallet_id: String,
    /// The deposited amount, in minor units (cents).
    pub amount: i64,
}

/// Payload of the event raised when money is debited from a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyWithdrawn {
    /// The debited wallet id.
    pub wallet_id: String,
    /// The withdrawn amount, in minor units (cents).
    pub amount: i64,
}

// ---------------------------------------------------------------------------
// The Wallet aggregate — `#[derive(AggregateRoot)]` over an embedded root.
// ---------------------------------------------------------------------------

/// The event-sourced **wallet aggregate**.
///
/// `#[derive(AggregateRoot)]` finds the embedded `firefly`
/// [`AggregateRoot`] field (`root`) and generates `Wallet::AGGREGATE_TYPE`
/// plus `aggregate()` / `aggregate_mut()` accessors. The projected state
/// (`owner`, `balance`, `opened`) is folded from the stream by [`apply`].
#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    /// The framework aggregate root — uncommitted-event buffer + version.
    pub root: AggregateRoot,
    /// The owner's display name.
    pub owner: String,
    /// The current balance as a [`Money`] value object.
    pub balance: Money,
    /// Whether the wallet has been opened (an empty stream is "absent").
    pub opened: bool,
}

impl Wallet {
    /// Rebuilds a wallet by folding `events` (its full ordered stream) — the
    /// canonical event-sourcing rehydration. An empty stream yields an
    /// unopened wallet.
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

    /// Opens a fresh wallet, raising a [`WalletOpened`] event.
    ///
    /// # Errors
    /// [`DomainError::OwnerRequired`] for an empty owner;
    /// [`DomainError::NonPositiveAmount`] for a negative opening balance (a
    /// zero opening balance is allowed).
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

    /// Credits `amount` to the wallet, raising a [`MoneyDeposited`] event.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when the wallet was never opened;
    /// [`DomainError::NonPositiveAmount`] when `amount` is not positive.
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

    /// Debits `amount` from the wallet, raising a [`MoneyWithdrawn`] event.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when never opened;
    /// [`DomainError::NonPositiveAmount`] when `amount` is not positive;
    /// [`DomainError::InsufficientFunds`] when `amount` exceeds the balance.
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

    /// Drains the events raised by the command methods, ready to hand to the
    /// [`EventStore`](firefly::eventsourcing::EventStore).
    pub fn take_uncommitted(&mut self) -> Vec<DomainEvent> {
        self.root.take_uncommitted()
    }

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
    /// embedded root under `event_type` — the discriminator supplied by the
    /// derive's generated `EVENT_TYPE` const, so it is never spelled as a bare
    /// string literal at the call sites.
    fn raise<P: Serialize>(&mut self, event_type: &str, payload: &P) {
        let bytes = serde_json::to_vec(payload).expect("domain event payload serialises");
        self.root.raise(event_type, bytes);
    }

    /// Folds one persisted event into the projected state during
    /// [`rehydrate`](Wallet::rehydrate).
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

    fn require_opened(&self) -> Result<(), DomainError> {
        if self.opened {
            Ok(())
        } else {
            Err(DomainError::NotFound(self.root.id.clone()))
        }
    }
}

/// The **read-model** projection of a wallet — the wire shape served by
/// `GET /api/v1/wallets/:id` and stored in the read-model repository.
///
/// It is a flat, query-optimised view rebuilt from the event stream by the
/// [`ledger`](crate::ledger) projection; the [`Wallet`] aggregate is the
/// write model, this is the read model (CQRS, book chapter 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WalletView {
    /// The wallet id.
    pub id: String,
    /// The owner's display name.
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied) — lets a client
    /// detect staleness under eventual consistency.
    pub version: i64,
}

/// A single wallet event in the **streaming** wire shape served by the
/// optional `GET /api/v1/wallets/:id/events` endpoint. It flattens a
/// [`DomainEvent`] into the members a client cares about, decoupling the HTTP
/// contract from the persisted envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletEvent {
    /// The wallet id the event belongs to.
    pub wallet_id: String,
    /// The 1-based stream version of this event.
    pub version: i64,
    /// The event type (`WalletOpened` / `MoneyDeposited` / `MoneyWithdrawn`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// The signed balance delta this event applied, in minor units: a deposit
    /// / opening balance is positive, a withdrawal negative.
    pub amount: i64,
}

impl WalletEvent {
    /// Projects a persisted [`DomainEvent`] into the streaming wire shape,
    /// decoding the signed `amount` from the event payload.
    pub fn from_domain(event: &DomainEvent) -> Self {
        let amount = match event.event_type.as_str() {
            WalletOpened::EVENT_TYPE => serde_json::from_slice::<WalletOpened>(&event.payload)
                .map(|p| p.opening_balance)
                .unwrap_or(0),
            MoneyDeposited::EVENT_TYPE => serde_json::from_slice::<MoneyDeposited>(&event.payload)
                .map(|p| p.amount)
                .unwrap_or(0),
            MoneyWithdrawn::EVENT_TYPE => serde_json::from_slice::<MoneyWithdrawn>(&event.payload)
                .map(|p| -p.amount)
                .unwrap_or(0),
            _ => 0,
        };
        WalletEvent {
            wallet_id: event.aggregate_id.clone(),
            version: event.version,
            event_type: event.event_type.clone(),
            amount,
        }
    }
}

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
    fn open_raises_one_event_at_version_1() {
        let mut w = Wallet::open("w1", "alice", Money::cents(500)).unwrap();
        let events = w.take_uncommitted();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, WalletOpened::EVENT_TYPE);
        assert_eq!(events[0].version, 1);
        assert_eq!(events[0].aggregate_type, AGGREGATE_TYPE);
        let payload: WalletOpened = serde_json::from_slice(&events[0].payload).unwrap();
        assert_eq!(payload.opening_balance, 500);
    }

    #[test]
    fn deposit_and_withdraw_update_balance_and_raise_events() {
        let mut w = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
        w.deposit(Money::cents(50)).unwrap();
        assert_eq!(w.balance, Money::cents(150));
        w.withdraw(Money::cents(30)).unwrap();
        assert_eq!(w.balance, Money::cents(120));
        let events = w.take_uncommitted();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].event_type, MoneyDeposited::EVENT_TYPE);
        assert_eq!(events[2].event_type, MoneyWithdrawn::EVENT_TYPE);
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
    fn commands_on_unopened_wallet_are_not_found() {
        let mut w = Wallet::rehydrate("ghost", &[]);
        assert_eq!(
            w.deposit(Money::cents(10)).unwrap_err(),
            DomainError::NotFound("ghost".into())
        );
    }

    #[test]
    fn rehydrate_folds_the_full_stream() {
        let mut writer = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
        writer.deposit(Money::cents(50)).unwrap();
        writer.withdraw(Money::cents(20)).unwrap();
        let stream = writer.take_uncommitted();

        let rebuilt = Wallet::rehydrate("w1", &stream);
        assert_eq!(rebuilt.balance, Money::cents(130));
        assert_eq!(rebuilt.owner, "alice");
        assert_eq!(rebuilt.root.version, 3);
        assert_eq!(Wallet::AGGREGATE_TYPE, "Wallet");
    }

    #[test]
    fn wallet_event_projects_signed_amounts() {
        let mut w = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
        w.deposit(Money::cents(40)).unwrap();
        w.withdraw(Money::cents(25)).unwrap();
        let stream = w.take_uncommitted();
        let projected: Vec<WalletEvent> = stream.iter().map(WalletEvent::from_domain).collect();
        assert_eq!(projected[0].amount, 100);
        assert_eq!(projected[1].amount, 40);
        assert_eq!(projected[2].amount, -25);
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
