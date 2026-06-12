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

//! CQRS **messages** and the [`Bank`] application service that executes
//! them — the write side of the CQRS split.
//!
//! The wire-shape command/query messages ([`OpenAccount`], [`Deposit`],
//! [`Withdraw`], [`GetAccount`]) implement [`firefly_cqrs::Message`] (with
//! validation), and [`register`] wires their handlers onto the
//! [`Bus`](firefly_cqrs::Bus). Each handler delegates to [`Bank`], the
//! shared application service that:
//!
//! 1. rehydrates the [`Account`](crate::domain::Account) aggregate from the
//!    [`EventStore`](firefly_eventsourcing::EventStore),
//! 2. runs the domain command,
//! 3. appends the new events with optimistic concurrency, and
//! 4. publishes each event to the EDA [`Broker`](firefly_eda::Broker) so the
//!    [`projections`](crate::projections) runner can update the read model.
//!
//! The same [`Bank`] is driven by the transfer [`saga`](crate::saga), so the
//! debit/credit legs go through the identical persist-and-publish path.

use std::sync::Arc;
use std::time::Duration;

use firefly_cqrs::{Bus, CqrsError, Message};
use firefly_eda::{Broker, Event};
use firefly_eventsourcing::{DomainEvent, EventStore};
use serde::{Deserialize, Serialize};

use crate::domain::{Account, AccountView, DomainError, AGGREGATE_TYPE};

/// The EDA topic every account domain event is published to. Glob
/// subscribers (`accounts.*`) and the projection runner key off it.
pub const EVENTS_TOPIC: &str = "accounts.events";

/// The logical EDA source stamped on published events.
pub const EVENT_SOURCE: &str = "reactive-banking";

/// How long [`GetAccount`] results stay in the CQRS query cache.
pub const GET_ACCOUNT_CACHE_TTL: Duration = Duration::from_secs(30);

// --------------------------------------------------------------------
// CQRS messages
// --------------------------------------------------------------------

/// `POST /api/v1/accounts` command — open a new account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAccount {
    /// The account owner's display name.
    pub owner: String,
    /// The opening balance, in minor units (cents); must be `>= 0`.
    #[serde(rename = "openingBalance")]
    pub opening_balance: i64,
}

impl Message for OpenAccount {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.owner.trim().is_empty() {
            return Err(CqrsError::validation("owner is required"));
        }
        if self.opening_balance < 0 {
            return Err(CqrsError::validation("openingBalance must be >= 0"));
        }
        Ok(())
    }
}

/// `POST /api/v1/accounts/:id/deposit` command — credit an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Deposit {
    /// The account to credit.
    #[serde(rename = "accountId")]
    pub account_id: String,
    /// The amount to credit, in minor units (cents); must be `> 0`.
    pub amount: i64,
}

impl Message for Deposit {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.account_id.is_empty() {
            return Err(CqrsError::validation("accountId is required"));
        }
        if self.amount <= 0 {
            return Err(CqrsError::validation("amount must be > 0"));
        }
        Ok(())
    }
}

/// `POST /api/v1/accounts/:id/withdraw` command — debit an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Withdraw {
    /// The account to debit.
    #[serde(rename = "accountId")]
    pub account_id: String,
    /// The amount to debit, in minor units (cents); must be `> 0`.
    pub amount: i64,
}

impl Message for Withdraw {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.account_id.is_empty() {
            return Err(CqrsError::validation("accountId is required"));
        }
        if self.amount <= 0 {
            return Err(CqrsError::validation("amount must be > 0"));
        }
        Ok(())
    }
}

/// `GET /api/v1/accounts/:id` query — fetch the read-model view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAccount {
    /// The account id to fetch.
    pub id: String,
}

impl Message for GetAccount {
    fn cache_ttl(&self) -> Option<Duration> {
        Some(GET_ACCOUNT_CACHE_TTL)
    }
}

// --------------------------------------------------------------------
// Bank — the application service
// --------------------------------------------------------------------

/// The shared **application service** behind every command and the transfer
/// saga: it owns the event store and the EDA broker, and turns a domain
/// command into persisted-and-published events.
///
/// Cloning is cheap (everything is behind an [`Arc`]). Both the CQRS
/// handlers and the saga legs call into it, so there is exactly one
/// persist-and-publish code path for the whole service.
#[derive(Clone)]
pub struct Bank {
    store: Arc<dyn EventStore>,
    broker: Arc<dyn Broker>,
}

impl Bank {
    /// Builds the service over an [`EventStore`] and an EDA [`Broker`].
    pub fn new(store: Arc<dyn EventStore>, broker: Arc<dyn Broker>) -> Self {
        Bank { store, broker }
    }

    /// The event store this service persists to (handy for the streaming
    /// events endpoint, which replays a stream directly).
    pub fn store(&self) -> &Arc<dyn EventStore> {
        &self.store
    }

    /// Opens a new account, persisting the `AccountOpened` event and
    /// publishing it. Returns the opened account's read-model view.
    pub async fn open(
        &self,
        owner: &str,
        opening_balance: i64,
    ) -> Result<AccountView, DomainError> {
        let id = new_account_id();
        let mut account = Account::open(&id, owner, opening_balance)?;
        self.commit(&mut account, 0).await?;
        Ok(account.view())
    }

    /// Credits `amount` to `account_id`, persisting + publishing the
    /// `MoneyDeposited` event.
    pub async fn deposit(&self, account_id: &str, amount: i64) -> Result<AccountView, DomainError> {
        let mut account = self.load(account_id).await?;
        let expected = account.root.version;
        account.deposit(amount)?;
        self.commit(&mut account, expected).await?;
        Ok(account.view())
    }

    /// Debits `amount` from `account_id`, persisting + publishing the
    /// `MoneyWithdrawn` event. Errors with
    /// [`DomainError::InsufficientFunds`] on overdraft (the saga's failure
    /// trigger).
    pub async fn withdraw(
        &self,
        account_id: &str,
        amount: i64,
    ) -> Result<AccountView, DomainError> {
        let mut account = self.load(account_id).await?;
        let expected = account.root.version;
        account.withdraw(amount)?;
        self.commit(&mut account, expected).await?;
        Ok(account.view())
    }

    /// Loads the full event stream for `account_id` (for the streaming
    /// events endpoint and the read-side query fallback).
    pub async fn load_events(&self, account_id: &str) -> Result<Vec<DomainEvent>, DomainError> {
        match self.store.load(account_id).await {
            Ok(events) => Ok(events),
            Err(firefly_eventsourcing::EventSourcingError::AggregateNotFound) => {
                Err(DomainError::NotFound(account_id.to_string()))
            }
            Err(e) => Err(DomainError::NotFound(format!("{account_id}: {e}"))),
        }
    }

    /// Rehydrates the aggregate from its persisted stream.
    async fn load(&self, account_id: &str) -> Result<Account, DomainError> {
        let events = self.load_events(account_id).await?;
        Ok(Account::rehydrate(account_id, &events))
    }

    /// Appends the aggregate's uncommitted events at `expected_version`
    /// (optimistic concurrency) then publishes each to the EDA broker.
    async fn commit(&self, account: &mut Account, expected: i64) -> Result<(), DomainError> {
        let events = account.take_uncommitted();
        if events.is_empty() {
            return Ok(());
        }
        self.store
            .append(&account.root.id, expected, events.clone())
            .await
            .map_err(|e| match e {
                firefly_eventsourcing::EventSourcingError::Concurrency => {
                    DomainError::NotFound(format!("{}: concurrent modification", account.root.id))
                }
                other => DomainError::NotFound(format!("{}: {other}", account.root.id)),
            })?;
        for event in &events {
            let envelope = to_event_envelope(event);
            // A publish failure must not corrupt the (already persisted)
            // stream; surface it as a not-found-flavoured error only when
            // truly fatal. The in-memory and Kafka brokers both succeed for
            // a healthy topic.
            self.broker
                .publish(envelope)
                .await
                .map_err(|e| DomainError::NotFound(format!("publish failed: {e}")))?;
        }
        Ok(())
    }
}

/// Maps a persisted [`DomainEvent`] onto the canonical EDA [`Event`]
/// envelope, carrying the JSON-encoded domain event as the payload and the
/// aggregate id as the partition key (so per-account events stay ordered on
/// a real Kafka topic).
pub fn to_event_envelope(event: &DomainEvent) -> Event {
    let payload = serde_json::to_vec(event).expect("domain event serializes");
    Event::new(
        EVENTS_TOPIC,
        event.event_type.clone(),
        EVENT_SOURCE,
        Some(payload),
    )
    .with_key(event.aggregate_id.clone().into_bytes())
    .with_header("aggregateType", AGGREGATE_TYPE)
    .with_header("aggregateId", event.aggregate_id.clone())
    .with_header("version", event.version.to_string())
}

/// Returns `acc_` + 24 lowercase hex characters, sourcing randomness from a
/// v4 UUID — the same id shape the orders sample uses for orders.
pub fn new_account_id() -> String {
    use std::fmt::Write as _;
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut id = String::with_capacity(28);
    id.push_str("acc_");
    for b in &bytes[..12] {
        let _ = write!(id, "{b:02x}");
    }
    id
}

/// Maps a [`DomainError`] onto the bus's [`CqrsError`] channel. The web
/// layer restores the precise HTTP status from the message (see
/// [`crate::web`]).
fn to_cqrs_error(e: DomainError) -> CqrsError {
    CqrsError::handler(e.to_string())
}

/// Wires the open/deposit/withdraw command handlers and the get-account
/// query handler onto `bus`, all backed by the shared [`Bank`] and the
/// read-model `repo`.
pub fn register(bus: &Bus, bank: Bank, repo: crate::repository::AccountRepository) {
    let open_bank = bank.clone();
    bus.register(move |cmd: OpenAccount| {
        let bank = open_bank.clone();
        async move {
            bank.open(&cmd.owner, cmd.opening_balance)
                .await
                .map_err(to_cqrs_error)
        }
    });

    let deposit_bank = bank.clone();
    bus.register(move |cmd: Deposit| {
        let bank = deposit_bank.clone();
        async move {
            bank.deposit(&cmd.account_id, cmd.amount)
                .await
                .map_err(to_cqrs_error)
        }
    });

    let withdraw_bank = bank.clone();
    bus.register(move |cmd: Withdraw| {
        let bank = withdraw_bank.clone();
        async move {
            bank.withdraw(&cmd.account_id, cmd.amount)
                .await
                .map_err(to_cqrs_error)
        }
    });

    // The read side is served from the projected read model, falling back
    // to folding the event stream when the projection has not yet caught up
    // (so a GET right after a write is never stale under eventual
    // consistency — the canonical "query the read model, repair from the
    // write model" pattern).
    bus.register(move |q: GetAccount| {
        let repo = Arc::clone(&repo);
        let bank = bank.clone();
        async move {
            if let Some(view) = repo
                .find_by_id(q.id.clone())
                .into_future()
                .await
                .map_err(|e| CqrsError::handler(e.to_string()))?
            {
                return Ok(view);
            }
            // Read model miss: fold the write-side stream.
            let events = bank.load_events(&q.id).await.map_err(to_cqrs_error)?;
            Ok(Account::rehydrate(&q.id, &events).view())
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use firefly_eda::InMemoryBroker;
    use firefly_eventsourcing::MemoryEventStore;

    use super::*;

    fn bank() -> Bank {
        Bank::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )
    }

    #[tokio::test]
    async fn open_deposit_withdraw_roundtrip_through_bank() {
        let bank = bank();
        let opened = bank.open("alice", 100).await.unwrap();
        assert_eq!(opened.balance, 100);
        assert_eq!(opened.version, 1);

        let after_deposit = bank.deposit(&opened.id, 50).await.unwrap();
        assert_eq!(after_deposit.balance, 150);
        assert_eq!(after_deposit.version, 2);

        let after_withdraw = bank.withdraw(&opened.id, 30).await.unwrap();
        assert_eq!(after_withdraw.balance, 120);
        assert_eq!(after_withdraw.version, 3);

        let events = bank.load_events(&opened.id).await.unwrap();
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn withdraw_overdraft_is_insufficient_funds() {
        let bank = bank();
        let opened = bank.open("bob", 40).await.unwrap();
        let err = bank.withdraw(&opened.id, 100).await.unwrap_err();
        assert_eq!(err, DomainError::InsufficientFunds);
    }

    #[tokio::test]
    async fn deposit_to_missing_account_is_not_found() {
        let bank = bank();
        let err = bank.deposit("acc_ghost", 10).await.unwrap_err();
        assert!(matches!(err, DomainError::NotFound(_)));
    }

    #[test]
    fn validation_rules() {
        assert_eq!(
            OpenAccount::default().validate().unwrap_err().to_string(),
            "owner is required"
        );
        assert_eq!(
            Deposit {
                account_id: "a".into(),
                amount: 0
            }
            .validate()
            .unwrap_err()
            .to_string(),
            "amount must be > 0"
        );
        assert!(GetAccount { id: "a".into() }.validate().is_ok());
        assert_eq!(
            GetAccount::default().cache_ttl(),
            Some(GET_ACCOUNT_CACHE_TTL)
        );
    }

    #[test]
    fn new_account_id_shape_is_unique() {
        let mut seen = HashSet::new();
        for _ in 0..256 {
            let id = new_account_id();
            assert_eq!(id.len(), 28, "id: {id}");
            assert!(id.starts_with("acc_"));
            assert!(id[4..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert!(seen.insert(id));
        }
    }

    #[test]
    fn open_account_wire_shape() {
        let json = serde_json::to_string(&OpenAccount {
            owner: "alice".into(),
            opening_balance: 100,
        })
        .unwrap();
        assert_eq!(json, r#"{"owner":"alice","openingBalance":100}"#);
    }

    #[tokio::test]
    async fn published_envelope_carries_key_and_headers() {
        let mut acc = Account::open("acc_x", "alice", 100).unwrap();
        let events = acc.take_uncommitted();
        let env = to_event_envelope(&events[0]);
        assert_eq!(env.topic, EVENTS_TOPIC);
        assert_eq!(env.event_type, "AccountOpened");
        assert_eq!(env.key, Some(b"acc_x".to_vec()));
        assert_eq!(env.headers.get("aggregateId").unwrap(), "acc_x");
        assert_eq!(env.headers.get("version").unwrap(), "1");
    }
}
