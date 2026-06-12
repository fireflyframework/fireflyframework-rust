//! Read-side handlers: the [`Projection`] port and the
//! [`ProjectionRunner`] dispatcher with replay support and global-stream
//! consumption.

use std::future::Future;
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

/// A quick [`Projection`] wrapping a single async handler — the Rust analog
/// of pyfly's `FunctionProjection`. Useful for tests and small read models
/// that do not warrant a dedicated type.
///
/// # Example
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use firefly_eventsourcing::{DomainEvent, FunctionProjection, Projection};
///
/// let seen = Arc::new(Mutex::new(Vec::<String>::new()));
/// let sink = Arc::clone(&seen);
/// let projection = FunctionProjection::new("audit", move |event: &DomainEvent| {
///     let sink = Arc::clone(&sink);
///     let ty = event.event_type.clone();
///     async move {
///         sink.lock().unwrap().push(ty);
///         Ok(())
///     }
/// });
/// assert_eq!(projection.name(), "audit");
/// ```
pub struct FunctionProjection<F> {
    name: String,
    handler: F,
}

impl<F, Fut> FunctionProjection<F>
where
    F: Fn(&DomainEvent) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), EventSourcingError>> + Send,
{
    /// Wraps `handler` as a projection named `name`.
    pub fn new(name: impl Into<String>, handler: F) -> Self {
        FunctionProjection {
            name: name.into(),
            handler,
        }
    }
}

#[async_trait]
impl<F, Fut> Projection for FunctionProjection<F>
where
    F: Fn(&DomainEvent) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), EventSourcingError>> + Send,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn apply(&self, event: &DomainEvent) -> Result<(), EventSourcingError> {
        (self.handler)(event).await
    }
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

    /// Processes one page of the global, cross-aggregate stream starting just
    /// after `after_event_id`, returning the cursor to resume from next.
    ///
    /// This is the building block of pyfly's `ProjectionRunner._loop`, exposed
    /// as a deterministic, single-step primitive (so tests need no sleeps and
    /// callers control the polling cadence). Semantics match pyfly exactly:
    ///
    /// * events are applied in global order, at-least-once;
    /// * the returned cursor advances only past **successfully** applied
    ///   events — on the first apply failure the batch stops there, leaving
    ///   the cursor on the last good event so the failed event is retried on
    ///   the next call (the cursor never jumps past an unprocessed event);
    /// * the apply error is returned alongside the advanced cursor so callers
    ///   can log/alert while still resuming correctly.
    ///
    /// When `tenant` is `Some`, only that tenant's events are consumed.
    /// Returns `(next_cursor, maybe_error)`: a `next_cursor` equal to the
    /// input `after_event_id` with no error means the stream is drained.
    pub async fn drive_once(
        &self,
        store: &dyn EventStore,
        after_event_id: Option<String>,
        batch_size: usize,
        tenant: Option<&str>,
    ) -> Result<(Option<String>, Option<EventSourcingError>), EventSourcingError> {
        let batch = store
            .stream_all(after_event_id.as_deref(), batch_size, tenant)
            .await?;
        let mut cursor = after_event_id;
        for streamed in &batch {
            match self.apply(&streamed.event).await {
                Ok(()) => cursor = Some(streamed.event_id.clone()),
                // Do NOT advance past a failed event — stop the batch here so
                // the cursor stays put and this event is retried next call.
                Err(err) => return Ok((cursor, Some(err))),
            }
        }
        Ok((cursor, None))
    }

    /// Drains the entire global stream once, from `start_after` to the end,
    /// applying every event to all registered projections in order. Pages the
    /// stream `batch_size` at a time. Returns the final cursor.
    ///
    /// Unlike [`replay`](ProjectionRunner::replay) (single aggregate), this
    /// rebuilds a read model that spans **all** aggregates — pyfly's
    /// projection-over-`stream_all` use case. The first apply error
    /// short-circuits and is propagated.
    pub async fn replay_all(
        &self,
        store: &dyn EventStore,
        start_after: Option<String>,
        batch_size: usize,
        tenant: Option<&str>,
    ) -> Result<Option<String>, EventSourcingError> {
        let batch_size = batch_size.max(1);
        let mut cursor = start_after;
        loop {
            let (next, err) = self
                .drive_once(store, cursor.clone(), batch_size, tenant)
                .await?;
            if let Some(err) = err {
                return Err(err);
            }
            // A cursor that did not move means the stream is drained.
            if next == cursor {
                return Ok(cursor);
            }
            cursor = next;
        }
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
