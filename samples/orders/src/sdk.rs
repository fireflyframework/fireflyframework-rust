//! Typed SDK over `/api/v1/orders` — the port of the Go sample's `sdk`
//! package (`orderssdk`), built on
//! [`firefly_client::RestClient`].
//!
//! Every request inherits the framework client's behaviour: JSON
//! encoding, `Accept: application/json`, correlation-id forwarding,
//! retry with backoff on 429/5xx, and RFC 7807 problem decoding into
//! [`FireflyError`](firefly_kernel::FireflyError) (surface it with
//! [`ClientError::as_firefly`]).

use firefly_client::{new_rest, ClientError, RestClient, NO_BODY};
use http::Method;

use crate::interfaces::{OrderDto, PlaceOrderRequest};

/// The typed SDK over `/api/v1/orders` — Go's `orderssdk.Client`.
#[derive(Debug, Clone)]
pub struct Client {
    rc: RestClient,
}

impl Client {
    /// Returns a client targeting `base_url` — Go's `orderssdk.New`.
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            rc: new_rest(base_url).build(),
        }
    }

    /// Posts a new order — Go's `Client.Place`.
    ///
    /// # Errors
    ///
    /// Non-2xx responses surface as [`ClientError::Problem`] carrying
    /// the decoded RFC 7807 problem (e.g. a 422 validation failure).
    pub async fn place(&self, req: &PlaceOrderRequest) -> Result<OrderDto, ClientError> {
        self.rc
            .request(Method::POST, "/api/v1/orders", Some(req))
            .await
    }

    /// Returns the order with the given id — Go's `Client.Get`.
    ///
    /// # Errors
    ///
    /// A missing order surfaces as [`ClientError::Problem`] with status
    /// 404 and the kernel's not-found type URI.
    pub async fn get(&self, id: &str) -> Result<OrderDto, ClientError> {
        self.rc
            .request(Method::GET, &format!("/api/v1/orders/{id}"), NO_BODY)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs_and_is_send_sync_clone() {
        fn assert_send_sync<T: Send + Sync + Clone>() {}
        assert_send_sync::<Client>();
        let client = Client::new("http://localhost:8080/");
        let _ = client.clone();
    }
}
