//! Wire shapes and CQRS messages — the port of the Go sample's
//! `interfaces` package (`ordersinterfaces`).
//!
//! These are the contract types shared by the [`core`](crate::core)
//! handlers, the [`web`](crate::web) endpoints, and the
//! [`sdk`](crate::sdk) client. JSON field names match the Go structs
//! byte-for-byte (`customer`, `sku`, `quantity`, `total`, `id`,
//! `status`, `createdAt`).

use std::time::Duration;

use chrono::{DateTime, Utc};
use firefly_cqrs::{CqrsError, Message};
use serde::{Deserialize, Serialize};

/// How long [`GetOrderQuery`] results stay in the CQRS query cache —
/// the Go sample's `GetOrderQuery.CacheTTL()` (30 s).
pub const GET_ORDER_CACHE_TTL: Duration = Duration::from_secs(30);

/// The wire-shape of `POST /api/v1/orders` — Go's `PlaceOrderRequest`.
///
/// Missing JSON members decode to their zero values (Go's lenient
/// `json.Decoder` behaviour), so domain validation — not decoding —
/// rejects incomplete orders.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaceOrderRequest {
    /// Customer placing the order.
    pub customer: String,
    /// Stock-keeping unit being ordered.
    pub sku: String,
    /// Number of units; must be positive.
    pub quantity: i64,
    /// Order total; must be positive.
    pub total: f64,
}

impl Message for PlaceOrderRequest {
    /// Runs domain validation — Go's `Validate()` implementing
    /// `cqrs.Validatable`. Picked up automatically by the
    /// [`ValidationMiddleware`](firefly_cqrs::ValidationMiddleware) the
    /// starter core installs on the bus.
    fn validate(&self) -> Result<(), CqrsError> {
        if self.customer.is_empty() {
            return Err(CqrsError::validation("customer is required"));
        }
        if self.sku.is_empty() {
            return Err(CqrsError::validation("sku is required"));
        }
        if self.quantity <= 0 {
            return Err(CqrsError::validation("quantity must be > 0"));
        }
        if self.total <= 0.0 {
            return Err(CqrsError::validation("total must be > 0"));
        }
        Ok(())
    }
}

/// The wire-shape returned by the orders endpoint — Go's `OrderDTO`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderDto {
    /// Order id (`ord_` + 24 hex characters).
    pub id: String,
    /// Customer who placed the order.
    pub customer: String,
    /// Stock-keeping unit ordered.
    pub sku: String,
    /// Number of units ordered.
    pub quantity: i64,
    /// Order total.
    pub total: f64,
    /// Lifecycle status; `"placed"` on creation.
    pub status: String,
    /// UTC creation instant (JSON `createdAt`, RFC 3339).
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

/// The query message for the GET endpoint — Go's `GetOrderQuery`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetOrderQuery {
    /// Id of the order to fetch.
    pub id: String,
}

impl Message for GetOrderQuery {
    /// Opts into 30 s read-side caching — Go's `CacheTTL()` implementing
    /// `cqrs.Cacheable`, honoured by
    /// [`QueryCache`](firefly_cqrs::QueryCache).
    fn cache_ttl(&self) -> Option<Duration> {
        Some(GET_ORDER_CACHE_TTL)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn valid_request() -> PlaceOrderRequest {
        PlaceOrderRequest {
            customer: "alice".into(),
            sku: "SKU-1".into(),
            quantity: 2,
            total: 19.99,
        }
    }

    #[test]
    fn validate_accepts_valid_request() {
        assert!(valid_request().validate().is_ok());
    }

    #[test]
    fn validate_rejects_missing_customer() {
        let req = PlaceOrderRequest {
            customer: String::new(),
            ..valid_request()
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err.to_string(), "customer is required");
    }

    #[test]
    fn validate_rejects_missing_sku() {
        let req = PlaceOrderRequest {
            sku: String::new(),
            ..valid_request()
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err.to_string(), "sku is required");
    }

    #[test]
    fn validate_rejects_non_positive_quantity() {
        for quantity in [0, -1] {
            let req = PlaceOrderRequest {
                quantity,
                ..valid_request()
            };
            let err = req.validate().unwrap_err();
            assert_eq!(err.to_string(), "quantity must be > 0");
        }
    }

    #[test]
    fn validate_rejects_non_positive_total() {
        for total in [0.0, -0.5] {
            let req = PlaceOrderRequest {
                total,
                ..valid_request()
            };
            let err = req.validate().unwrap_err();
            assert_eq!(err.to_string(), "total must be > 0");
        }
    }

    #[test]
    fn validation_errors_are_cqrs_validation_variant() {
        let err = PlaceOrderRequest::default().validate().unwrap_err();
        assert!(matches!(err, CqrsError::Validation(_)));
    }

    #[test]
    fn get_order_query_caches_for_30s() {
        let q = GetOrderQuery { id: "x".into() };
        assert_eq!(q.cache_ttl(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn place_order_request_is_not_cacheable() {
        assert_eq!(valid_request().cache_ttl(), None);
    }

    /// Missing members decode to zero values, like Go's `json.Decoder`.
    #[test]
    fn place_order_request_decodes_missing_members_to_defaults() {
        let req: PlaceOrderRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(req, PlaceOrderRequest::default());
    }

    /// The JSON member names match the Go wire shape byte-for-byte.
    #[test]
    fn place_order_request_wire_shape() {
        let json = serde_json::to_string(&valid_request()).unwrap();
        assert_eq!(
            json,
            r#"{"customer":"alice","sku":"SKU-1","quantity":2,"total":19.99}"#
        );
    }

    /// `OrderDTO` serializes in Go field order with `createdAt` camelCase
    /// and an RFC 3339 timestamp — the exact bytes the Go sample emits.
    #[test]
    fn order_dto_wire_shape() {
        let dto = OrderDto {
            id: "ord_0123456789abcdef01234567".into(),
            customer: "alice".into(),
            sku: "SKU-1".into(),
            quantity: 2,
            total: 19.99,
            status: "placed".into(),
            created_at: Utc.with_ymd_and_hms(2026, 6, 12, 10, 30, 0).unwrap(),
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert_eq!(
            json,
            r#"{"id":"ord_0123456789abcdef01234567","customer":"alice","sku":"SKU-1","quantity":2,"total":19.99,"status":"placed","createdAt":"2026-06-12T10:30:00Z"}"#
        );
        let back: OrderDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dto);
    }

    #[test]
    fn get_order_query_wire_shape() {
        let json = serde_json::to_string(&GetOrderQuery { id: "o1".into() }).unwrap();
        assert_eq!(json, r#"{"id":"o1"}"#);
    }
}
