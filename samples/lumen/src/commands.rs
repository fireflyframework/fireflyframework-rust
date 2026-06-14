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

//! The **CQRS** command/query split — declarative messages and their handlers
//! (book chapter 7, "CQRS").
//!
//! The write-side commands ([`OpenWallet`], [`Deposit`], [`Withdraw`]) and the
//! read-side query ([`GetWallet`]) are plain structs carrying
//! `#[derive(Command)]` / `#[derive(Query)]`, which generate their
//! `firefly::cqrs::Message` impls (with the `#[firefly(validate)]` required-field
//! checks and the query `cache_ttl`). Each handler is a free `async fn` marked
//! `#[command_handler]` / `#[query_handler]`, which generates a
//! `register_<fn>(bus)` helper.
//!
//! Free fns cannot capture wiring state, so the resolved [`Ledger`] +
//! [`ReadModel`] are published once at startup through [`bind`] and the
//! handlers reach them through [`state`] — the same pattern the
//! `macro-quickstart` sample uses. The handlers turn the bus's `CqrsError`
//! channel into the precise HTTP status at the web boundary.

use std::sync::{Arc, OnceLock};

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Wallet, WalletView};
use crate::ledger::{Ledger, ReadModel};
use crate::money::Money;

// ---------------------------------------------------------------------------
// Handler state — the resolved collaborators the free-fn handlers operate on.
// ---------------------------------------------------------------------------

/// The collaborators the CQRS handlers need: the write-side [`Ledger`] and the
/// read-side [`ReadModel`] the projection feeds.
struct HandlerState {
    ledger: Ledger,
    read_model: Arc<ReadModel>,
}

static STATE: OnceLock<HandlerState> = OnceLock::new();

/// Publishes the handlers' collaborators and returns the **effective** state —
/// the one actually held by the process-global `OnceLock`.
///
/// Because the global binds only once, a second call (a second
/// [`build_app`](crate::web::build_app) in the same test binary) keeps the
/// *first* ledger + read model. Returning the effective pair lets the
/// composition root wire the rest of the app (the controller's saga, the
/// projection) against the **same** collaborators the free-fn handlers use, so
/// the whole app stays consistent no matter how many times it is built.
pub fn bind(ledger: Ledger, read_model: Arc<ReadModel>) -> (Ledger, Arc<ReadModel>) {
    let effective = STATE.get_or_init(|| HandlerState { ledger, read_model });
    (effective.ledger.clone(), Arc::clone(&effective.read_model))
}

fn state() -> &'static HandlerState {
    STATE
        .get()
        .expect("commands::bind must run before a handler dispatches")
}

/// Maps a [`DomainError`] onto the bus's [`CqrsError`] channel. The web layer
/// restores the precise HTTP status from the detail message.
fn to_cqrs(e: DomainError) -> CqrsError {
    CqrsError::handler(e.to_string())
}

// ---------------------------------------------------------------------------
// CQRS messages — `#[derive(Command)]` / `#[derive(Query)]`.
// ---------------------------------------------------------------------------

/// `POST /api/v1/wallets` command — open a new wallet. `#[firefly(validate)]`
/// makes an empty `owner` fail validation before the handler runs.
///
/// It also derives [`Builder`](firefly::Builder) (Lombok `@Builder`), so a
/// caller can construct it fluently — `OpenWallet::builder().owner("ada").build()`
/// — with `opening_balance` defaulting to zero.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command, Builder)]
#[serde(default)]
pub struct OpenWallet {
    /// The wallet owner's display name — required.
    #[firefly(validate)]
    #[builder(into)]
    pub owner: String,
    /// The opening balance, in minor units (cents); must be `>= 0`.
    #[serde(rename = "openingBalance")]
    #[builder(default)]
    pub opening_balance: i64,
}

/// `POST /api/v1/wallets/:id/deposit` command — credit a wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct Deposit {
    /// The wallet to credit — required.
    #[firefly(validate)]
    #[serde(rename = "walletId")]
    pub wallet_id: String,
    /// The amount to credit, in minor units (cents); must be `> 0`.
    #[firefly(validate)]
    pub amount: i64,
}

/// `POST /api/v1/wallets/:id/withdraw` command — debit a wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct Withdraw {
    /// The wallet to debit — required.
    #[firefly(validate)]
    #[serde(rename = "walletId")]
    pub wallet_id: String,
    /// The amount to debit, in minor units (cents); must be `> 0`.
    #[firefly(validate)]
    pub amount: i64,
}

/// `GET /api/v1/wallets/:id` query. `#[firefly(cache_ttl = "30s")]` is reflected
/// on the generated `Message::cache_ttl`, so a `QueryCache` memoises reads for
/// 30 seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    /// The wallet id to fetch.
    pub id: String,
}

// ---------------------------------------------------------------------------
// CQRS handlers — `#[command_handler]` / `#[query_handler]`.
// ---------------------------------------------------------------------------

/// Handles [`OpenWallet`]. `#[command_handler]` generates
/// `register_open_wallet(bus)`.
#[command_handler]
pub async fn open_wallet(cmd: OpenWallet) -> Result<WalletView, CqrsError> {
    if cmd.opening_balance < 0 {
        return Err(CqrsError::validation("openingBalance must be >= 0"));
    }
    state()
        .ledger
        .open(&cmd.owner, Money::cents(cmd.opening_balance))
        .await
        .map_err(to_cqrs)
}

/// Handles [`Deposit`]. `#[command_handler]` generates `register_deposit(bus)`.
#[command_handler]
pub async fn deposit(cmd: Deposit) -> Result<WalletView, CqrsError> {
    state()
        .ledger
        .deposit(&cmd.wallet_id, Money::cents(cmd.amount))
        .await
        .map_err(to_cqrs)
}

/// Handles [`Withdraw`]. `#[command_handler]` generates `register_withdraw(bus)`.
#[command_handler]
pub async fn withdraw(cmd: Withdraw) -> Result<WalletView, CqrsError> {
    state()
        .ledger
        .withdraw(&cmd.wallet_id, Money::cents(cmd.amount))
        .await
        .map_err(to_cqrs)
}

/// Handles [`GetWallet`]. `#[query_handler]` generates `register_get_wallet(bus)`.
///
/// It serves from the projected read model, falling back to folding the event
/// stream when the projection has not yet caught up — the canonical "query the
/// read model, repair from the write model" pattern that keeps a read after a
/// write from going stale under eventual consistency.
#[query_handler]
pub async fn get_wallet(q: GetWallet) -> Result<WalletView, CqrsError> {
    if let Some(view) = state().read_model.find(&q.id) {
        return Ok(view);
    }
    let events = state().ledger.load_events(&q.id).await.map_err(to_cqrs)?;
    Ok(Wallet::rehydrate(&q.id, &events).view())
}

/// Installs every generated handler-registration helper on `bus`. The
/// composition root calls this after [`bind`].
pub fn register(bus: &Bus) {
    register_open_wallet(bus);
    register_deposit(bus);
    register_withdraw(bus);
    register_get_wallet(bus);
}

#[cfg(test)]
mod tests {
    use firefly::eda::InMemoryBroker;
    use firefly::eventsourcing::MemoryEventStore;

    use super::*;

    #[test]
    fn open_wallet_validates_owner() {
        assert!(OpenWallet::default().validate().is_err());
        assert!(OpenWallet {
            owner: "alice".into(),
            opening_balance: 0,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn deposit_validates_required_fields() {
        assert!(Deposit::default().validate().is_err());
        assert!(
            Deposit {
                wallet_id: "wlt_1".into(),
                amount: 0,
            }
            .validate()
            .is_err(),
            "zero amount fails the #[firefly(validate)] check"
        );
        assert!(Deposit {
            wallet_id: "wlt_1".into(),
            amount: 10,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn get_wallet_carries_cache_ttl() {
        assert!(GetWallet::default().cache_ttl().is_some());
    }

    #[test]
    fn open_wallet_wire_shape() {
        let json = serde_json::to_string(&OpenWallet {
            owner: "alice".into(),
            opening_balance: 100,
        })
        .unwrap();
        assert_eq!(json, r#"{"owner":"alice","openingBalance":100}"#);
    }

    #[tokio::test]
    async fn handlers_dispatch_through_the_bus() {
        let ledger = Ledger::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        );
        bind(ledger, Arc::new(ReadModel::default()));
        let bus = Bus::new();
        // Validation middleware enforces the `#[firefly(validate)]` checks.
        bus.use_middleware(firefly::cqrs::ValidationMiddleware::new());
        register(&bus);

        let opened: WalletView = bus
            .send(OpenWallet {
                owner: "alice".into(),
                opening_balance: 100,
            })
            .await
            .unwrap();
        assert_eq!(opened.balance, 100);

        let after: WalletView = bus
            .send(Deposit {
                wallet_id: opened.id.clone(),
                amount: 50,
            })
            .await
            .unwrap();
        assert_eq!(after.balance, 150);

        let fetched: WalletView = bus
            .query(GetWallet {
                id: opened.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(fetched.id, opened.id);
    }

    /// `#[derive(Builder)]` (Lombok `@Builder`) gives `OpenWallet` a fluent
    /// constructor: `owner` is required (`into` setter), `opening_balance`
    /// defaults to zero.
    #[test]
    fn open_wallet_builder_constructs_with_defaults() {
        let cmd = OpenWallet::builder().owner("ada").build().unwrap();
        assert_eq!(cmd.owner, "ada");
        assert_eq!(cmd.opening_balance, 0);

        let funded = OpenWallet::builder()
            .owner("bob")
            .opening_balance(5_000)
            .build()
            .unwrap();
        assert_eq!(funded.opening_balance, 5_000);

        // The required `owner` errors when omitted.
        assert!(OpenWallet::builder().build().is_err());
    }
}
