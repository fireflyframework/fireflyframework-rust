//! Persistence shapes and the repository port — the port of the Go
//! sample's `models` package (`ordersmodels`).

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// The persistence shape of an order entity — Go's `Order`.
#[derive(Debug, Clone, PartialEq)]
pub struct Order {
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
    /// UTC creation instant.
    pub created_at: DateTime<Utc>,
}

/// The order persistence error family. The single variant is the Rust
/// spelling of Go's `ErrNotFound` sentinel (`errors.New("orders: not
/// found")`) — its display string matches the Go message exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryError {
    /// The canonical missing-order error — Go's `ErrNotFound`.
    #[error("orders: not found")]
    NotFound,
}

/// The order persistence boundary — Go's `Repository` interface.
///
/// Object-safe (`Arc<dyn Repository>`) so the in-memory reference
/// implementation and any production store plug in interchangeably,
/// following the framework's ports-and-adapters convention.
#[async_trait]
pub trait Repository: Send + Sync {
    /// Persists `order`, returning the stored entity.
    async fn save(&self, order: Order) -> Result<Order, RepositoryError>;

    /// Returns the order with the given id, or
    /// [`RepositoryError::NotFound`].
    async fn get(&self, id: &str) -> Result<Order, RepositoryError>;
}

/// The in-process [`Repository`] implementation — Go's
/// `MemoryRepository` (`sync.RWMutex` + map).
#[derive(Debug, Default)]
pub struct MemoryRepository {
    store: RwLock<HashMap<String, Order>>,
}

impl MemoryRepository {
    /// Returns an empty repository — Go's `NewMemoryRepository()`.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Repository for MemoryRepository {
    async fn save(&self, order: Order) -> Result<Order, RepositoryError> {
        self.store
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(order.id.clone(), order.clone());
        Ok(order)
    }

    async fn get(&self, id: &str) -> Result<Order, RepositoryError> {
        self.store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or(RepositoryError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn order(id: &str) -> Order {
        Order {
            id: id.into(),
            customer: "alice".into(),
            sku: "SKU-1".into(),
            quantity: 2,
            total: 19.99,
            status: "placed".into(),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn save_then_get_roundtrips() {
        let repo = MemoryRepository::new();
        let saved = repo.save(order("o1")).await.unwrap();
        assert_eq!(saved.id, "o1");
        let got = repo.get("o1").await.unwrap();
        assert_eq!(got, saved);
    }

    #[tokio::test]
    async fn get_missing_returns_not_found() {
        let repo = MemoryRepository::new();
        let err = repo.get("missing").await.unwrap_err();
        assert_eq!(err, RepositoryError::NotFound);
    }

    #[tokio::test]
    async fn save_overwrites_existing_id() {
        let repo = MemoryRepository::new();
        repo.save(order("o1")).await.unwrap();
        let mut updated = order("o1");
        updated.status = "shipped".into();
        repo.save(updated.clone()).await.unwrap();
        assert_eq!(repo.get("o1").await.unwrap(), updated);
    }

    /// Go's `ErrNotFound` message, verbatim.
    #[test]
    fn not_found_displays_go_sentinel_message() {
        assert_eq!(RepositoryError::NotFound.to_string(), "orders: not found");
    }

    /// The repository is safe to share across tasks — Go's
    /// `sync.RWMutex` contract.
    #[tokio::test]
    async fn repository_is_shareable_across_tasks() {
        let repo = Arc::new(MemoryRepository::new());
        let mut handles = Vec::new();
        for i in 0..8 {
            let repo = Arc::clone(&repo);
            handles.push(tokio::spawn(async move {
                repo.save(order(&format!("o{i}"))).await.unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        for i in 0..8 {
            assert!(repo.get(&format!("o{i}")).await.is_ok());
        }
    }

    #[test]
    fn types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Order>();
        assert_send_sync::<MemoryRepository>();
        assert_send_sync::<RepositoryError>();
    }
}
