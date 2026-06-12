//! CQRS handler registration — the port of the Go sample's `core`
//! package (`orderscore`).
//!
//! [`register`] wires two handlers onto the bus:
//!
//! - `PlaceOrderRequest -> OrderDto` — assigns an id, stamps the
//!   creation time, persists through the [`Repository`] port, and
//!   returns the DTO view.
//! - `GetOrderQuery -> OrderDto` — looks the order up; a repository
//!   miss surfaces as a handler error whose message is Go's
//!   `kernel.NewNotFound("order <id> not found")` detail. (The Rust
//!   bus's typed error channel is [`CqrsError`], so the web layer
//!   restores the 404 status — see
//!   [`crate::web`].)

use std::fmt::Write as _;
use std::sync::Arc;

use chrono::Utc;
use firefly_cqrs::{Bus, CqrsError};

use crate::interfaces::{GetOrderQuery, OrderDto, PlaceOrderRequest};
use crate::models::{Order, Repository, RepositoryError};

/// Wires the orders handlers onto `bus` — Go's `orderscore.Register`.
pub fn register(bus: &Bus, repo: Arc<dyn Repository>) {
    let save_repo = Arc::clone(&repo);
    bus.register(move |req: PlaceOrderRequest| {
        let repo = Arc::clone(&save_repo);
        async move {
            let order = Order {
                id: new_id(),
                customer: req.customer,
                sku: req.sku,
                quantity: req.quantity,
                total: req.total,
                status: "placed".into(),
                created_at: Utc::now(),
            };
            let saved = repo
                .save(order)
                .await
                .map_err(|e| CqrsError::handler(e.to_string()))?;
            Ok::<_, CqrsError>(to_dto(saved))
        }
    });

    bus.register(move |q: GetOrderQuery| {
        let repo = Arc::clone(&repo);
        async move {
            match repo.get(&q.id).await {
                Ok(order) => Ok::<_, CqrsError>(to_dto(order)),
                // Go: kernel.NewNotFound("order " + q.ID + " not found").
                Err(RepositoryError::NotFound) => {
                    Err(CqrsError::handler(format!("order {} not found", q.id)))
                }
            }
        }
    });
}

/// Maps the persistence shape onto the wire shape — Go's `toDTO`.
fn to_dto(order: Order) -> OrderDto {
    OrderDto {
        id: order.id,
        customer: order.customer,
        sku: order.sku,
        quantity: order.quantity,
        total: order.total,
        status: order.status,
        created_at: order.created_at,
    }
}

/// Returns `ord_` + 24 lowercase hex characters — the Go sample's
/// `newID()` (`"ord_" + hex(12 random bytes)`), sourcing randomness
/// from a v4 UUID.
fn new_id() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut id = String::with_capacity(28);
    id.push_str("ord_");
    for b in &bytes[..12] {
        let _ = write!(id, "{b:02x}");
    }
    id
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use firefly_cqrs::ValidationMiddleware;

    use crate::models::MemoryRepository;

    use super::*;

    fn wired_bus() -> Bus {
        let bus = Bus::new();
        register(&bus, Arc::new(MemoryRepository::new()));
        bus
    }

    fn place_request() -> PlaceOrderRequest {
        PlaceOrderRequest {
            customer: "alice".into(),
            sku: "SKU-1".into(),
            quantity: 2,
            total: 19.99,
        }
    }

    #[tokio::test]
    async fn place_order_persists_and_returns_dto() {
        let bus = wired_bus();
        let before = Utc::now();
        let dto: OrderDto = bus.send(place_request()).await.unwrap();
        assert!(dto.id.starts_with("ord_"), "id: {}", dto.id);
        assert_eq!(dto.customer, "alice");
        assert_eq!(dto.sku, "SKU-1");
        assert_eq!(dto.quantity, 2);
        assert_eq!(dto.total, 19.99);
        assert_eq!(dto.status, "placed");
        assert!(dto.created_at >= before && dto.created_at <= Utc::now());
    }

    #[tokio::test]
    async fn get_order_returns_placed_order() {
        let bus = wired_bus();
        let placed: OrderDto = bus.send(place_request()).await.unwrap();
        let got: OrderDto = bus
            .query(GetOrderQuery {
                id: placed.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(got, placed);
    }

    /// Go: the handler returns `kernel.NewNotFound("order <id> not
    /// found")`; the Rust bus carries the same detail as a handler error.
    #[tokio::test]
    async fn get_missing_order_maps_to_handler_error() {
        let bus = wired_bus();
        let err = bus
            .query::<GetOrderQuery, OrderDto>(GetOrderQuery {
                id: "missing".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, CqrsError::Handler(_)), "err: {err:?}");
        assert_eq!(err.to_string(), "order missing not found");
    }

    /// With the validation middleware installed (as the starter core
    /// does), an invalid command never reaches the handler.
    #[tokio::test]
    async fn validation_middleware_rejects_invalid_request() {
        let bus = Bus::new();
        bus.use_middleware(ValidationMiddleware::new());
        register(&bus, Arc::new(MemoryRepository::new()));
        let err = bus
            .send::<PlaceOrderRequest, OrderDto>(PlaceOrderRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(err, CqrsError::Validation(_)));
        assert_eq!(err.to_string(), "customer is required");
    }

    /// Go's `newID()` contract: `ord_` + 24 lowercase hex characters.
    #[test]
    fn new_id_has_go_shape_and_is_unique() {
        let mut seen = HashSet::new();
        for _ in 0..256 {
            let id = new_id();
            assert_eq!(id.len(), 28, "id: {id}");
            assert!(id.starts_with("ord_"), "id: {id}");
            assert!(
                id[4..]
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "id: {id}"
            );
            assert!(seen.insert(id), "duplicate id generated");
        }
    }
}
