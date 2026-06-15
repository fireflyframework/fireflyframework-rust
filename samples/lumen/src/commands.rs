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

//! The **CQRS** command/query split — declarative messages and their handler
//! **bean** (book chapter 7, "CQRS").
//!
//! The write-side commands ([`OpenWallet`], [`Deposit`], [`Withdraw`]) and the
//! read-side query ([`GetWallet`]) are plain structs carrying
//! `#[derive(Command)]` / `#[derive(Query)]`, which generate their
//! `firefly::cqrs::Message` impls (with the `#[firefly(validate)]` required-field
//! checks and the query `cache_ttl`).
//!
//! The handlers live on a **DI bean** — [`WalletHandlers`], a
//! `#[derive(Service)]` whose collaborators (the write-side [`Ledger`] and the
//! read-side [`ReadModel`]) are `#[autowired]` from the container. `#[handlers]`
//! registers each `#[command_handler]` / `#[query_handler]` method on the bus, so
//! a handler reaches its collaborators through `self` — Spring Boot's
//! `@Component` command handler, with **no process-global** and no composition
//! root. The handlers turn the bus's `CqrsError` channel into the precise HTTP
//! status at the web boundary.

use std::sync::Arc;

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Wallet, WalletView};
use crate::ledger::{Ledger, ReadModel};
use crate::money::Money;

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
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command, Builder, Schema)]
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
// CQRS handler bean — a `#[derive(Service)]` whose methods are the command /
// query handlers, autowiring the `Ledger` + `ReadModel` from the container.
// ---------------------------------------------------------------------------

/// The CQRS **handler bean** — Spring's `@Component` command/query handler. Its
/// collaborators are `#[autowired]` from the DI container: the write-side
/// [`Ledger`] every command drives, and the read-side [`ReadModel`] the
/// projection feeds and `GetWallet` serves. `#[handlers]` registers each method
/// on the bus, so the handler reaches its collaborators through `self` — no
/// process-global, no composition root.
#[derive(Service)]
struct WalletHandlers {
    /// The write-side application service (autowired).
    #[autowired]
    ledger: Arc<Ledger>,
    /// The read-side projection store the `GetWallet` query reads (autowired).
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletHandlers {
    /// Handles [`OpenWallet`].
    #[command_handler]
    async fn open_wallet(&self, cmd: OpenWallet) -> Result<WalletView, CqrsError> {
        if cmd.opening_balance < 0 {
            return Err(CqrsError::validation("openingBalance must be >= 0"));
        }
        self.ledger
            .open(&cmd.owner, Money::cents(cmd.opening_balance))
            .await
            .map_err(to_cqrs)
    }

    /// Handles [`Deposit`].
    #[command_handler]
    async fn deposit(&self, cmd: Deposit) -> Result<WalletView, CqrsError> {
        self.ledger
            .deposit(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }

    /// Handles [`Withdraw`].
    #[command_handler]
    async fn withdraw(&self, cmd: Withdraw) -> Result<WalletView, CqrsError> {
        self.ledger
            .withdraw(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }

    /// Handles [`GetWallet`] — serve from the projected read model, falling back
    /// to folding the event stream when the projection has not yet caught up
    /// (the "query the read model, repair from the write model" pattern that
    /// keeps a read after a write from going stale under eventual consistency).
    #[query_handler]
    async fn get_wallet(&self, q: GetWallet) -> Result<WalletView, CqrsError> {
        if let Some(view) = self.read_model.find(&q.id) {
            return Ok(view);
        }
        let events = self.ledger.load_events(&q.id).await.map_err(to_cqrs)?;
        Ok(Wallet::rehydrate(&q.id, &events).view())
    }
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

    /// The handler **bean** operates on its `#[autowired]` collaborators: each
    /// method drives the same `Ledger` + `ReadModel` the container would inject,
    /// with no process-global. (The bus wiring is covered end-to-end by the HTTP
    /// tests, which boot the full `FireflyApplication`.)
    #[tokio::test]
    async fn handler_bean_operates_on_its_autowired_collaborators() {
        let handlers = WalletHandlers {
            ledger: Arc::new(Ledger::new(
                Arc::new(MemoryEventStore::new()),
                Arc::new(InMemoryBroker::new()),
            )),
            read_model: Arc::new(ReadModel::default()),
        };

        let opened = handlers
            .open_wallet(OpenWallet {
                owner: "alice".into(),
                opening_balance: 100,
            })
            .await
            .unwrap();
        assert_eq!(opened.balance, 100);

        let after = handlers
            .deposit(Deposit {
                wallet_id: opened.id.clone(),
                amount: 50,
            })
            .await
            .unwrap();
        assert_eq!(after.balance, 150);

        let fetched = handlers
            .get_wallet(GetWallet {
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
