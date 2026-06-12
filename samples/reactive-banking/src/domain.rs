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

//! The banking **domain** — the event-sourced [`Account`] aggregate, its
//! domain-event payloads, the read-model [`AccountView`], and the typed
//! [`DomainError`] family.
//!
//! This is the heart of the sample: an [`Account`] is reconstructed by
//! folding the [`firefly_eventsourcing::DomainEvent`] stream produced by
//! its command methods ([`Account::open`], [`Account::deposit`],
//! [`Account::withdraw`]). Every command method validates the invariant
//! (positive amounts, sufficient funds), then [`raise`](AggregateRoot::raise)s
//! the corresponding event and mutates in-memory state — the canonical
//! event-sourcing shape the framework's
//! [`AggregateRoot`](firefly_eventsourcing::AggregateRoot) is built for.
//!
//! ```text
//!   open ──► AccountOpened ──► balance = initial
//!   deposit(amount) ──► MoneyDeposited ──► balance += amount
//!   withdraw(amount) ──► MoneyWithdrawn ──► balance -= amount   (if balance ≥ amount)
//! ```
//!
//! Amounts are integer **minor units** (cents) so the arithmetic is exact
//! — no floating-point drift in money math. The wire shape exposes them as
//! the `balance` / `amount` integer JSON members.

use chrono::{DateTime, Utc};
use firefly_eventsourcing::{AggregateRoot, DomainEvent};
use serde::{Deserialize, Serialize};

/// The aggregate-type discriminator stamped onto every [`DomainEvent`] an
/// [`Account`] raises (`firefly_eventsourcing` records it on the stream).
pub const AGGREGATE_TYPE: &str = "Account";

/// Event-type discriminator: an account was opened.
pub const EVENT_ACCOUNT_OPENED: &str = "AccountOpened";
/// Event-type discriminator: money was deposited.
pub const EVENT_MONEY_DEPOSITED: &str = "MoneyDeposited";
/// Event-type discriminator: money was withdrawn.
pub const EVENT_MONEY_WITHDRAWN: &str = "MoneyWithdrawn";

/// The typed domain-error family. The `Display` strings are stable so they
/// can be asserted on (and surface verbatim as RFC 7807 problem `detail`s
/// once mapped at the HTTP boundary).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// A deposit / withdrawal / opening balance amount was not strictly
    /// positive.
    #[error("amount must be positive")]
    NonPositiveAmount,
    /// A withdrawal (or saga debit) exceeded the available balance.
    #[error("insufficient funds")]
    InsufficientFunds,
    /// An operation referenced an account that does not exist.
    #[error("account {0} not found")]
    NotFound(String),
    /// The owner name was empty when opening an account.
    #[error("owner is required")]
    OwnerRequired,
}

/// The JSON payload of an [`EVENT_ACCOUNT_OPENED`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountOpened {
    /// The new account's id.
    pub account_id: String,
    /// The account owner's display name.
    pub owner: String,
    /// The opening balance, in minor units (cents).
    pub opening_balance: i64,
}

/// The JSON payload of an [`EVENT_MONEY_DEPOSITED`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyDeposited {
    /// The credited account id.
    pub account_id: String,
    /// The deposited amount, in minor units (cents).
    pub amount: i64,
}

/// The JSON payload of an [`EVENT_MONEY_WITHDRAWN`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyWithdrawn {
    /// The debited account id.
    pub account_id: String,
    /// The withdrawn amount, in minor units (cents).
    pub amount: i64,
}

/// The event-sourced **account aggregate**.
///
/// Holds a [`firefly_eventsourcing::AggregateRoot`] (the framework's
/// event-buffer + version tracker, the Rust analog of Go struct embedding)
/// plus the projected in-memory state (`owner`, `balance`). Command methods
/// validate, [`raise`](AggregateRoot::raise) an event, and apply it; the
/// caller then drains [`take_uncommitted`](AggregateRoot::take_uncommitted)
/// to persist through the [`EventStore`](firefly_eventsourcing::EventStore).
#[derive(Debug, Clone)]
pub struct Account {
    /// The framework aggregate root — event buffer + version.
    pub root: AggregateRoot,
    /// The account owner's display name.
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// Whether the account has been opened (an empty stream is "absent").
    pub opened: bool,
}

impl Account {
    /// Rebuilds an account by folding `events` (its full ordered stream),
    /// the canonical event-sourcing rehydration — `load(id)` then
    /// `Account::rehydrate(id, events)`. An empty stream yields an
    /// unopened account.
    pub fn rehydrate(id: &str, events: &[DomainEvent]) -> Self {
        let mut account = Account {
            root: AggregateRoot::new(id, AGGREGATE_TYPE),
            owner: String::new(),
            balance: 0,
            opened: false,
        };
        for event in events {
            account.apply(event);
            // Keep the root version in lock-step with the stream head so a
            // subsequent command appends at the right expected version.
            account.root.version = event.version;
        }
        account
    }

    /// Opens a fresh account with `owner` and `opening_balance` (cents),
    /// raising an [`EVENT_ACCOUNT_OPENED`] event.
    ///
    /// # Errors
    ///
    /// [`DomainError::OwnerRequired`] when `owner` is empty;
    /// [`DomainError::NonPositiveAmount`] when `opening_balance` is negative
    /// (a zero opening balance is allowed).
    pub fn open(
        id: impl Into<String>,
        owner: impl Into<String>,
        opening_balance: i64,
    ) -> Result<Self, DomainError> {
        let id = id.into();
        let owner = owner.into();
        if owner.trim().is_empty() {
            return Err(DomainError::OwnerRequired);
        }
        if opening_balance < 0 {
            return Err(DomainError::NonPositiveAmount);
        }
        let mut account = Account {
            root: AggregateRoot::new(&id, AGGREGATE_TYPE),
            owner: owner.clone(),
            balance: 0,
            opened: false,
        };
        let payload = AccountOpened {
            account_id: id.clone(),
            owner,
            opening_balance,
        };
        account.raise(EVENT_ACCOUNT_OPENED, &payload);
        account.balance = opening_balance;
        account.opened = true;
        Ok(account)
    }

    /// Credits `amount` (cents) to the account, raising an
    /// [`EVENT_MONEY_DEPOSITED`] event.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotFound`] when the account was never opened;
    /// [`DomainError::NonPositiveAmount`] when `amount <= 0`.
    pub fn deposit(&mut self, amount: i64) -> Result<(), DomainError> {
        self.require_opened()?;
        if amount <= 0 {
            return Err(DomainError::NonPositiveAmount);
        }
        let payload = MoneyDeposited {
            account_id: self.root.id.clone(),
            amount,
        };
        self.raise(EVENT_MONEY_DEPOSITED, &payload);
        self.balance += amount;
        Ok(())
    }

    /// Debits `amount` (cents) from the account, raising an
    /// [`EVENT_MONEY_WITHDRAWN`] event.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotFound`] when the account was never opened;
    /// [`DomainError::NonPositiveAmount`] when `amount <= 0`;
    /// [`DomainError::InsufficientFunds`] when `amount` exceeds the balance.
    pub fn withdraw(&mut self, amount: i64) -> Result<(), DomainError> {
        self.require_opened()?;
        if amount <= 0 {
            return Err(DomainError::NonPositiveAmount);
        }
        if amount > self.balance {
            return Err(DomainError::InsufficientFunds);
        }
        let payload = MoneyWithdrawn {
            account_id: self.root.id.clone(),
            amount,
        };
        self.raise(EVENT_MONEY_WITHDRAWN, &payload);
        self.balance -= amount;
        Ok(())
    }

    /// Drains the events raised by command methods, ready to hand to
    /// [`EventStore::append`](firefly_eventsourcing::EventStore::append).
    pub fn take_uncommitted(&mut self) -> Vec<DomainEvent> {
        self.root.take_uncommitted()
    }

    /// The current read-model view of this aggregate.
    pub fn view(&self) -> AccountView {
        AccountView {
            id: self.root.id.clone(),
            owner: self.owner.clone(),
            balance: self.balance,
            version: self.root.version,
        }
    }

    /// Serializes `payload` and raises an `event_type` event onto the root.
    fn raise<P: Serialize>(&mut self, event_type: &str, payload: &P) {
        let bytes = serde_json::to_vec(payload).expect("domain event payload serializes");
        self.root.raise(event_type, bytes);
    }

    /// Folds one persisted event into the projected state during
    /// [`rehydrate`](Account::rehydrate).
    fn apply(&mut self, event: &DomainEvent) {
        match event.event_type.as_str() {
            EVENT_ACCOUNT_OPENED => {
                if let Ok(p) = serde_json::from_slice::<AccountOpened>(&event.payload) {
                    self.owner = p.owner;
                    self.balance = p.opening_balance;
                    self.opened = true;
                }
            }
            EVENT_MONEY_DEPOSITED => {
                if let Ok(p) = serde_json::from_slice::<MoneyDeposited>(&event.payload) {
                    self.balance += p.amount;
                }
            }
            EVENT_MONEY_WITHDRAWN => {
                if let Ok(p) = serde_json::from_slice::<MoneyWithdrawn>(&event.payload) {
                    self.balance -= p.amount;
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

/// The **read-model** projection of an account — the wire shape served by
/// `GET /api/v1/accounts/:id` and stored in the
/// [`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository).
///
/// It is a flat, query-optimised view rebuilt from the event stream by the
/// [`projections`](crate::projections) runner; the [`Account`] aggregate is
/// the write model, this is the read model (CQRS).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountView {
    /// The account id.
    pub id: String,
    /// The account owner's display name.
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied) — lets a client
    /// detect staleness.
    pub version: i64,
}

/// A single account event in the **streaming** wire shape served by
/// `GET /api/v1/accounts/:id/events` (NDJSON / SSE). It flattens a
/// [`DomainEvent`] into the members a client cares about, decoupling the
/// HTTP contract from the persisted envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountEvent {
    /// The account id the event belongs to.
    pub account_id: String,
    /// The 1-based stream version of this event.
    pub version: i64,
    /// The event type (`AccountOpened` / `MoneyDeposited` / `MoneyWithdrawn`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// The signed balance delta this event applied, in minor units: a
    /// deposit / opening balance is positive, a withdrawal negative.
    pub amount: i64,
    /// When the event was raised (UTC, RFC 3339).
    pub time: DateTime<Utc>,
}

impl AccountEvent {
    /// Projects a persisted [`DomainEvent`] into the streaming wire shape,
    /// decoding the signed `amount` from the event payload.
    pub fn from_domain(event: &DomainEvent) -> Self {
        let amount = match event.event_type.as_str() {
            EVENT_ACCOUNT_OPENED => serde_json::from_slice::<AccountOpened>(&event.payload)
                .map(|p| p.opening_balance)
                .unwrap_or(0),
            EVENT_MONEY_DEPOSITED => serde_json::from_slice::<MoneyDeposited>(&event.payload)
                .map(|p| p.amount)
                .unwrap_or(0),
            EVENT_MONEY_WITHDRAWN => serde_json::from_slice::<MoneyWithdrawn>(&event.payload)
                .map(|p| -p.amount)
                .unwrap_or(0),
            _ => 0,
        };
        AccountEvent {
            account_id: event.aggregate_id.clone(),
            version: event.version,
            event_type: event.event_type.clone(),
            amount,
            time: event.time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_validates_owner_and_balance() {
        assert_eq!(
            Account::open("a1", "  ", 100).unwrap_err(),
            DomainError::OwnerRequired
        );
        assert_eq!(
            Account::open("a1", "alice", -1).unwrap_err(),
            DomainError::NonPositiveAmount
        );
        let acc = Account::open("a1", "alice", 0).unwrap();
        assert_eq!(acc.balance, 0);
        assert!(acc.opened);
    }

    #[test]
    fn open_raises_one_event_at_version_1() {
        let mut acc = Account::open("a1", "alice", 500).unwrap();
        let events = acc.take_uncommitted();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EVENT_ACCOUNT_OPENED);
        assert_eq!(events[0].version, 1);
        assert_eq!(events[0].aggregate_type, AGGREGATE_TYPE);
        let payload: AccountOpened = serde_json::from_slice(&events[0].payload).unwrap();
        assert_eq!(payload.opening_balance, 500);
        assert_eq!(payload.owner, "alice");
    }

    #[test]
    fn deposit_and_withdraw_update_balance_and_raise_events() {
        let mut acc = Account::open("a1", "alice", 100).unwrap();
        acc.deposit(50).unwrap();
        assert_eq!(acc.balance, 150);
        acc.withdraw(30).unwrap();
        assert_eq!(acc.balance, 120);
        let events = acc.take_uncommitted();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].event_type, EVENT_MONEY_DEPOSITED);
        assert_eq!(events[1].version, 2);
        assert_eq!(events[2].event_type, EVENT_MONEY_WITHDRAWN);
        assert_eq!(events[2].version, 3);
    }

    #[test]
    fn deposit_rejects_non_positive_amount() {
        let mut acc = Account::open("a1", "alice", 100).unwrap();
        assert_eq!(acc.deposit(0).unwrap_err(), DomainError::NonPositiveAmount);
        assert_eq!(acc.deposit(-5).unwrap_err(), DomainError::NonPositiveAmount);
    }

    #[test]
    fn withdraw_rejects_overdraft() {
        let mut acc = Account::open("a1", "alice", 100).unwrap();
        assert_eq!(
            acc.withdraw(101).unwrap_err(),
            DomainError::InsufficientFunds
        );
        // The failed command raised no event beyond the open.
        assert_eq!(acc.root.uncommitted().len(), 1);
    }

    #[test]
    fn commands_on_unopened_account_are_not_found() {
        let mut acc = Account::rehydrate("ghost", &[]);
        assert_eq!(
            acc.deposit(10).unwrap_err(),
            DomainError::NotFound("ghost".into())
        );
        assert_eq!(
            acc.withdraw(10).unwrap_err(),
            DomainError::NotFound("ghost".into())
        );
    }

    #[test]
    fn rehydrate_folds_the_full_stream() {
        let mut writer = Account::open("a1", "alice", 100).unwrap();
        writer.deposit(50).unwrap();
        writer.withdraw(20).unwrap();
        let stream = writer.take_uncommitted();

        let rebuilt = Account::rehydrate("a1", &stream);
        assert_eq!(rebuilt.balance, 130);
        assert_eq!(rebuilt.owner, "alice");
        assert_eq!(rebuilt.root.version, 3);
        assert!(rebuilt.opened);
        // A command on the rebuilt aggregate appends at version 4.
        let mut rebuilt = rebuilt;
        rebuilt.deposit(10).unwrap();
        let next = rebuilt.take_uncommitted();
        assert_eq!(next[0].version, 4);
    }

    #[test]
    fn account_event_projects_signed_amounts() {
        let mut acc = Account::open("a1", "alice", 100).unwrap();
        acc.deposit(40).unwrap();
        acc.withdraw(25).unwrap();
        let stream = acc.take_uncommitted();
        let projected: Vec<AccountEvent> = stream.iter().map(AccountEvent::from_domain).collect();
        assert_eq!(projected[0].amount, 100); // opening
        assert_eq!(projected[1].amount, 40); // deposit
        assert_eq!(projected[2].amount, -25); // withdrawal (negative)
        assert_eq!(projected[0].event_type, EVENT_ACCOUNT_OPENED);
    }

    #[test]
    fn account_view_wire_shape() {
        let acc = Account::open("acc_1", "alice", 250).unwrap();
        let json = serde_json::to_string(&acc.view()).unwrap();
        assert_eq!(
            json,
            r#"{"id":"acc_1","owner":"alice","balance":250,"version":1}"#
        );
    }
}
