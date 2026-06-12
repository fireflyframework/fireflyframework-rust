//! Typed **reactive SDK** over the banking API, built on
//! [`firefly_client::WebClient`] (the Spring `WebClient` analog).
//!
//! Every mutating call attaches the JWT bearer token; the read and the
//! streaming-events calls hand back [`Mono`] / [`Flux`] so the caller
//! composes them with the full Reactor operator set. The streaming
//! [`BankClient::stream_events`] decodes the `application/x-ndjson`
//! response **lazily as a `Flux`**, the headline reactive-consumer path.

use firefly_client::{WebClient, WebClientBuilder};
use firefly_reactive::{Flux, Mono};
use http::Method;

use crate::commands::OpenAccount;
use crate::domain::{AccountEvent, AccountView};
use crate::saga::{TransferRequest, TransferResult};

/// A typed reactive client over the reactive-banking API.
///
/// Construct it with a base URL and a bearer token ([`BankClient::new`]);
/// every request carries `Authorization: Bearer <token>` so the mutating
/// routes (open / deposit / withdraw / transfer) are authorized. The
/// terminal operators return [`Mono`] / [`Flux`] — nothing runs until the
/// publisher is subscribed / blocked / awaited.
#[derive(Clone)]
pub struct BankClient {
    web: WebClient,
}

impl BankClient {
    /// Builds a client targeting `base_url`, authenticating every request
    /// with `token` (mint one via
    /// [`crate::security::mint_token`]).
    pub fn new(base_url: impl AsRef<str>, token: &str) -> Self {
        let web = WebClientBuilder::new(base_url)
            .with_header("Authorization", format!("Bearer {token}"))
            .build();
        BankClient { web }
    }

    /// Builds a client from a pre-configured [`WebClient`] (e.g. one whose
    /// `reqwest::Client` routes to an in-process `tower` server in tests).
    pub fn from_web_client(web: WebClient) -> Self {
        BankClient { web }
    }

    /// `POST /api/v1/accounts` — opens an account, returning its view as a
    /// [`Mono`].
    pub fn open_account(&self, owner: &str, opening_balance: i64) -> Mono<AccountView> {
        self.web
            .method(Method::POST)
            .uri("/api/v1/accounts")
            .body(&OpenAccount {
                owner: owner.to_owned(),
                opening_balance,
            })
            .retrieve()
            .body_to_mono::<AccountView>()
    }

    /// `POST /api/v1/accounts/:id/deposit` — credits `amount`, returning the
    /// updated view as a [`Mono`].
    pub fn deposit(&self, account_id: &str, amount: i64) -> Mono<AccountView> {
        self.web
            .method(Method::POST)
            .uri(format!("/api/v1/accounts/{account_id}/deposit"))
            .body(&serde_json::json!({ "amount": amount }))
            .retrieve()
            .body_to_mono::<AccountView>()
    }

    /// `POST /api/v1/accounts/:id/withdraw` — debits `amount`, returning the
    /// updated view as a [`Mono`].
    pub fn withdraw(&self, account_id: &str, amount: i64) -> Mono<AccountView> {
        self.web
            .method(Method::POST)
            .uri(format!("/api/v1/accounts/{account_id}/withdraw"))
            .body(&serde_json::json!({ "amount": amount }))
            .retrieve()
            .body_to_mono::<AccountView>()
    }

    /// `POST /api/v1/transfers` — runs a money transfer (saga), returning
    /// the [`TransferResult`] as a [`Mono`].
    pub fn transfer(&self, from: &str, to: &str, amount: i64) -> Mono<TransferResult> {
        self.web
            .method(Method::POST)
            .uri("/api/v1/transfers")
            .body(&TransferRequest {
                from: from.to_owned(),
                to: to.to_owned(),
                amount,
            })
            .retrieve()
            .body_to_mono::<TransferResult>()
    }

    /// `GET /api/v1/accounts/:id` — fetches the read-model view as a
    /// [`Mono`].
    pub fn get_account(&self, account_id: &str) -> Mono<AccountView> {
        self.web
            .method(Method::GET)
            .uri(format!("/api/v1/accounts/{account_id}"))
            .retrieve()
            .body_to_mono::<AccountView>()
    }

    /// `GET /api/v1/accounts/:id/events` — consumes the **reactive
    /// streaming** events endpoint as a [`Flux<AccountEvent>`].
    ///
    /// The `application/x-ndjson` response is decoded lazily, one
    /// [`AccountEvent`] per line as it arrives, with backpressure — the
    /// reactive-consumer counterpart of the server's reactive push.
    pub fn stream_events(&self, account_id: &str) -> Flux<AccountEvent> {
        self.web
            .method(Method::GET)
            .uri(format!("/api/v1/accounts/{account_id}/events"))
            .header("Accept", "application/x-ndjson")
            .retrieve()
            .body_to_flux::<AccountEvent>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_is_send_sync_clone() {
        fn assert_send_sync<T: Send + Sync + Clone>() {}
        assert_send_sync::<BankClient>();
        let token = crate::security::mint_token("u1", &["CUSTOMER"]);
        let client = BankClient::new("http://localhost:8080", &token);
        let _ = client.clone();
    }
}
