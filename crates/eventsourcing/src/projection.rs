//! Read-side handlers: the [`Projection`] port and the
//! [`ProjectionRunner`] dispatcher with replay support.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::aggregate::{DomainEvent, EventStore};
use crate::error::EventSourcingError;

/// Projection is a read-side handler that consumes events to build a query
/// model. Implementations must be idempotent — events may be replayed
/// during recovery.
#[async_trait]
pub trait Projection: Send + Sync {
    /// Stable, human-readable identifier of the projection.
    fn name(&self) -> &str;

    /// Folds `event` into the read model. Errors short-circuit the runner.
    async fn apply(&self, event: &DomainEvent) -> Result<(), EventSourcingError>;
}

/// ProjectionRunner dispatches events to a set of projections. Used by the
/// starter to build read models on the fly during normal operation and to
/// rebuild them from scratch on demand.
#[derive(Default)]
pub struct ProjectionRunner {
    projections: RwLock<Vec<Arc<dyn Projection>>>,
}

impl ProjectionRunner {
    /// Returns an empty runner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a projection. Projections are applied in registration order.
    pub fn register(&self, projection: Arc<dyn Projection>) {
        let mut projections = self.projections.write().expect("runner lock poisoned");
        projections.push(projection);
    }

    /// Dispatches `event` to every registered projection. The first error
    /// short-circuits.
    pub async fn apply(&self, event: &DomainEvent) -> Result<(), EventSourcingError> {
        let projections: Vec<Arc<dyn Projection>> = {
            let guard = self.projections.read().expect("runner lock poisoned");
            guard.clone()
        };
        for projection in projections {
            projection.apply(event).await?;
        }
        Ok(())
    }

    /// Re-applies all events in `aggregate_id`'s stream — used by the
    /// rebuild admin endpoint to recover a corrupted read model.
    pub async fn replay(
        &self,
        store: &dyn EventStore,
        aggregate_id: &str,
    ) -> Result<(), EventSourcingError> {
        let events = store.load(aggregate_id).await?;
        for event in &events {
            self.apply(event).await?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for ProjectionRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self
            .projections
            .read()
            .expect("runner lock poisoned")
            .iter()
            .map(|p| p.name().to_string())
            .collect();
        f.debug_struct("ProjectionRunner")
            .field("projections", &names)
            .finish()
    }
}
