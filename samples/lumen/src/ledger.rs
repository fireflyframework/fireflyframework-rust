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

//! The **event-sourced ledger** — the application service that persists wallet
//! events and the EDA-driven read-model **projection** (book chapters 8
//! "Domain Events", 9 "Event Sourcing", 10 "Messaging").
//!
//! [`Ledger`] is the single write path the CQRS command handlers and the
//! transfer saga both call. For every command it:
//!
//! 1. rehydrates the [`Wallet`] aggregate from the
//!    [`EventStore`](firefly::eventsourcing::EventStore),
//! 2. runs the domain command,
//! 3. appends the new events with **optimistic concurrency**, and
//! 4. publishes each event to the EDA [`Broker`](firefly::eda::Broker) so the
//!    [`WalletProjection`] projection updates the read model.
//!
//! The projection is a `#[derive(Service)]` bean whose `#[event_listener]`
//! method closes the CQRS loop: it consumes a published event, rebuilds the
//! wallet view from its full stream, and upserts it into the [`ReadModel`].
//! Rebuilding from the stream (rather than mutating the row from the single
//! event) keeps the projection **idempotent** — an at-least-once redelivery
//! converges on the same view.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use firefly::eda::{Broker, Event};
use firefly::eventsourcing::{DomainEvent, EventSourcingError, EventStore};
use firefly::prelude::*;

use crate::domain::{DomainError, Wallet, WalletView, AGGREGATE_TYPE};
use crate::money::Money;

/// The EDA topic every wallet domain event is published to. The projection
/// and any external subscriber key off it.
pub const EVENTS_TOPIC: &str = "wallets.events";

/// The logical EDA source stamped on published events.
pub const EVENT_SOURCE: &str = "lumen";

// ---------------------------------------------------------------------------
// Read model — the CQRS query side.
// ---------------------------------------------------------------------------

/// The in-memory **read model**: a map of wallet id → [`WalletView`], upserted
/// by the projection and served by the `GetWallet` query. A real service would
/// back this with `firefly`'s reactive repository over Postgres; an in-memory
/// map keeps the teaching baseline dependency-free.
#[derive(Debug, Default)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}

impl ReadModel {
    /// Upserts a projected view, replacing any previous row for the id.
    pub fn upsert(&self, view: WalletView) {
        self.rows
            .lock()
            .expect("read model lock")
            .insert(view.id.clone(), view);
    }

    /// Looks a projected view up by id.
    pub fn find(&self, id: &str) -> Option<WalletView> {
        self.rows.lock().expect("read model lock").get(id).cloned()
    }
}

// ---------------------------------------------------------------------------
// Ledger — the write-side application service.
// ---------------------------------------------------------------------------

/// The shared **application service** behind every command and the transfer
/// saga: it owns the event store and the EDA broker, and turns a domain
/// command into persisted-and-published events. Cloning is cheap (everything
/// is behind an [`Arc`]).
#[derive(Clone)]
pub struct Ledger {
    store: Arc<dyn EventStore>,
    broker: Arc<dyn Broker>,
}

impl Ledger {
    /// Builds the ledger over an [`EventStore`] and an EDA [`Broker`].
    pub fn new(store: Arc<dyn EventStore>, broker: Arc<dyn Broker>) -> Self {
        Ledger { store, broker }
    }

    /// The event store this ledger persists to (used by the streaming events
    /// endpoint, which replays a stream directly, and by the projection).
    pub fn store(&self) -> &Arc<dyn EventStore> {
        &self.store
    }

    /// The EDA broker this ledger publishes to (used by the composition root to
    /// subscribe the read-model projection to the very broker the ledger
    /// publishes on).
    pub fn broker(&self) -> &Arc<dyn Broker> {
        &self.broker
    }

    /// Opens a new wallet, persisting + publishing the `WalletOpened` event.
    /// Returns the opened wallet's read-model view.
    pub async fn open(&self, owner: &str, opening: Money) -> Result<WalletView, DomainError> {
        let id = new_wallet_id();
        let mut wallet = Wallet::open(&id, owner, opening)?;
        self.commit(&mut wallet, 0).await?;
        Ok(wallet.view())
    }

    /// Credits `amount` to `wallet_id`, persisting + publishing
    /// `MoneyDeposited`.
    pub async fn deposit(&self, wallet_id: &str, amount: Money) -> Result<WalletView, DomainError> {
        let mut wallet = self.load(wallet_id).await?;
        let expected = wallet.root.version;
        wallet.deposit(amount)?;
        self.commit(&mut wallet, expected).await?;
        Ok(wallet.view())
    }

    /// Debits `amount` from `wallet_id`, persisting + publishing
    /// `MoneyWithdrawn`. Errors with [`DomainError::InsufficientFunds`] on
    /// overdraft — the transfer saga's failure trigger.
    pub async fn withdraw(
        &self,
        wallet_id: &str,
        amount: Money,
    ) -> Result<WalletView, DomainError> {
        let mut wallet = self.load(wallet_id).await?;
        let expected = wallet.root.version;
        wallet.withdraw(amount)?;
        self.commit(&mut wallet, expected).await?;
        Ok(wallet.view())
    }

    /// Loads the full event stream for `wallet_id` (for the streaming endpoint
    /// and the read-side query fallback).
    pub async fn load_events(&self, wallet_id: &str) -> Result<Vec<DomainEvent>, DomainError> {
        match self.store.load(wallet_id).await {
            Ok(events) => Ok(events),
            Err(EventSourcingError::AggregateNotFound) => {
                Err(DomainError::NotFound(wallet_id.to_string()))
            }
            Err(e) => Err(DomainError::NotFound(format!("{wallet_id}: {e}"))),
        }
    }

    /// Rehydrates the aggregate from its persisted stream.
    async fn load(&self, wallet_id: &str) -> Result<Wallet, DomainError> {
        let events = self.load_events(wallet_id).await?;
        Ok(Wallet::rehydrate(wallet_id, &events))
    }

    /// Appends the aggregate's uncommitted events at `expected_version`
    /// (optimistic concurrency) then publishes each to the EDA broker.
    async fn commit(&self, wallet: &mut Wallet, expected: i64) -> Result<(), DomainError> {
        let events = wallet.take_uncommitted();
        if events.is_empty() {
            return Ok(());
        }
        self.store
            .append(&wallet.root.id, expected, events.clone())
            .await
            .map_err(|e| match e {
                EventSourcingError::Concurrency => {
                    DomainError::NotFound(format!("{}: concurrent modification", wallet.root.id))
                }
                other => DomainError::NotFound(format!("{}: {other}", wallet.root.id)),
            })?;
        for event in &events {
            self.broker
                .publish(to_envelope(event))
                .await
                .map_err(|e| DomainError::NotFound(format!("publish failed: {e}")))?;
        }
        Ok(())
    }
}

/// Maps a persisted [`DomainEvent`] onto the canonical EDA [`Event`] envelope,
/// carrying the JSON-encoded domain event as the payload and the wallet id as
/// the partition key (so per-wallet events stay ordered on a real broker).
pub fn to_envelope(event: &DomainEvent) -> Event {
    let payload = serde_json::to_vec(event).expect("domain event serialises");
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

/// Returns `wlt_` + 24 lowercase hex characters, sourcing randomness from a
/// v4 UUID.
pub fn new_wallet_id() -> String {
    use std::fmt::Write as _;
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut id = String::with_capacity(28);
    id.push_str("wlt_");
    for b in &bytes[..12] {
        let _ = write!(id, "{b:02x}");
    }
    id
}

// ---------------------------------------------------------------------------
// Projection — the EDA listener **bean** that feeds the read model.
// ---------------------------------------------------------------------------

/// The read-model **projection bean** — Spring's `@Component @EventListener`. It
/// `#[autowired]`s the [`Ledger`] (for the event store it replays) and the
/// [`ReadModel`] it feeds, and `#[handlers]` subscribes its [`project`] method
/// to [`EVENTS_TOPIC`] on the very broker the ledger publishes to. For each
/// delivered event it reloads the affected wallet's stream, folds it into a
/// [`WalletView`], and upserts it — the idempotent rebuild-from-stream
/// projection that closes the CQRS loop, wired entirely through the DI container
/// with no process-global.
///
/// [`project`]: WalletProjection::project
#[derive(Service)]
struct WalletProjection {
    /// The application service whose event store the projection replays
    /// (autowired).
    #[autowired]
    ledger: Arc<Ledger>,
    /// The read model the projection upserts (autowired) — the same instance the
    /// `GetWallet` query reads.
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletProjection {
    /// Projects one delivered wallet event into the read model.
    #[event_listener(topic = "wallets.events")]
    async fn project(&self, ev: Event) -> FireflyResult<()> {
        let Some(wallet_id) = ev.headers.get("aggregateId") else {
            return Ok(());
        };
        // A transient store miss is swallowed so one poison message never stalls
        // the projection — the EDA at-least-once contract.
        if let Ok(events) = self.ledger.store().load(wallet_id).await {
            let view = Wallet::rehydrate(wallet_id, &events).view();
            self.read_model.upsert(view);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use firefly::eda::InMemoryBroker;
    use firefly::eventsourcing::MemoryEventStore;

    use super::*;

    fn ledger() -> Ledger {
        Ledger::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )
    }

    #[tokio::test]
    async fn open_deposit_withdraw_roundtrip() {
        let ledger = ledger();
        let opened = ledger.open("alice", Money::cents(100)).await.unwrap();
        assert_eq!(opened.balance, 100);
        assert_eq!(opened.version, 1);

        let after_deposit = ledger.deposit(&opened.id, Money::cents(50)).await.unwrap();
        assert_eq!(after_deposit.balance, 150);

        let after_withdraw = ledger.withdraw(&opened.id, Money::cents(30)).await.unwrap();
        assert_eq!(after_withdraw.balance, 120);
        assert_eq!(after_withdraw.version, 3);

        let events = ledger.load_events(&opened.id).await.unwrap();
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn withdraw_overdraft_is_insufficient_funds() {
        let ledger = ledger();
        let opened = ledger.open("bob", Money::cents(40)).await.unwrap();
        let err = ledger
            .withdraw(&opened.id, Money::cents(100))
            .await
            .unwrap_err();
        assert_eq!(err, DomainError::InsufficientFunds);
    }

    #[tokio::test]
    async fn deposit_to_missing_wallet_is_not_found() {
        let ledger = ledger();
        let err = ledger
            .deposit("wlt_ghost", Money::cents(10))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound(_)));
    }

    #[tokio::test]
    async fn published_envelope_carries_key_and_headers() {
        let mut w = Wallet::open("wlt_x", "alice", Money::cents(100)).unwrap();
        let events = w.take_uncommitted();
        let env = to_envelope(&events[0]);
        assert_eq!(env.topic, EVENTS_TOPIC);
        assert_eq!(env.event_type, "WalletOpened");
        assert_eq!(env.key, Some(b"wlt_x".to_vec()));
        assert_eq!(env.headers.get("aggregateId").unwrap(), "wlt_x");
        assert_eq!(env.headers.get("version").unwrap(), "1");
    }

    #[test]
    fn wallet_id_shape_is_unique_and_hex() {
        let a = new_wallet_id();
        let b = new_wallet_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 28);
        assert!(a.starts_with("wlt_"));
        assert!(a[4..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
