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

//! Outbound domain-event publishing — pyfly's `pyfly.cqrs.event`
//! (`@publish_domain_event` / `CommandEventPublisher` /
//! `EdaCommandEventPublisher` / `NoOpEventPublisher` / `EventFailureStrategy`)
//! and the command-bus step that harvests a command/result's domain events
//! and forwards each to the EDA broker after a successful dispatch.
//!
//! # The two halves
//!
//! 1. **What to publish.** A command (or its result) exposes the events it
//!    produced by implementing [`DomainEvents`]; each emitted value
//!    implements [`DomainEvent`] (an `event_type` + a JSON payload), the Rust
//!    spelling of pyfly reading `result.domain_events` / `command.domain_events`
//!    and adapting each to the EDA `publish(destination, event_type, payload)`
//!    contract.
//! 2. **Where to publish.** A [`CommandEventPublisher`] sends each event to a
//!    destination. [`EdaCommandEventPublisher`] forwards to a
//!    [`firefly_eda::Publisher`]; [`NoOpEventPublisher`] silently drops them
//!    (the default when no EDA integration is wired).
//!
//! # Wiring it into the bus
//!
//! Install a [`DomainEventMiddleware`] on the [`Bus`](crate::Bus): after a
//! command of type `C` dispatches successfully, the middleware reads `C`'s
//! [`DomainEvents`] and publishes each one through the configured publisher,
//! honouring the [`EventFailureStrategy`]. Result-side harvesting — pyfly's
//! `result.domain_events` — is available through
//! [`Bus::send_publishing`](crate::Bus::send_publishing), which publishes a
//! result implementing [`DomainEvents`] after dispatch.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_cqrs::{
//!     Bus, CqrsError, DomainEvent, DomainEventMiddleware,
//!     EdaCommandEventPublisher, Message,
//! };
//! use firefly_eda::InMemoryBroker;
//! use serde::Serialize;
//!
//! #[derive(Clone, Serialize)]
//! struct PlaceOrder { id: String }
//!
//! // The command exposes the events it produced via the Message hook.
//! impl Message for PlaceOrder {
//!     fn domain_events(&self) -> Vec<DomainEvent> {
//!         vec![DomainEvent::new("OrderPlaced", serde_json::json!({ "id": self.id }))]
//!     }
//! }
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let broker = Arc::new(InMemoryBroker::new());
//! let publisher = Arc::new(EdaCommandEventPublisher::new(broker));
//!
//! let bus = Bus::new();
//! bus.use_middleware(DomainEventMiddleware::new(publisher));
//! bus.register(|_c: PlaceOrder| async move { Ok::<_, CqrsError>(()) });
//!
//! let _: () = bus.send(PlaceOrder { id: "o1".into() }).await.unwrap();
//! // "OrderPlaced" was published to "cqrs.events" after the handler ran.
//! # });
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use firefly_eda::{Event, Publisher};
use serde::Serialize;

use crate::bus::{DynHandler, Envelope, HandlerFuture, Middleware};
use crate::CqrsError;

/// The default destination domain events are published to when a command
/// handler does not declare its own — pyfly's `EdaCommandEventPublisher`
/// `default_destination="cqrs.events"`.
pub const DEFAULT_EVENT_DESTINATION: &str = "cqrs.events";

/// A single domain event ready to be published — an `event_type` plus a JSON
/// payload, the Rust spelling of pyfly serialising a domain-event object to
/// `(event_type, payload)` for the EDA `publish(...)` contract.
///
/// Construct one with [`DomainEvent::new`], or from a serializable value with
/// [`DomainEvent::from_value`] (which serialises it to the payload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainEvent {
    /// Logical event type, e.g. `OrderPlaced` — becomes the EDA event's
    /// `type` field.
    pub event_type: String,
    /// JSON payload encoded into the EDA event body.
    pub payload: serde_json::Value,
}

impl DomainEvent {
    /// Builds an event from an explicit type and JSON payload.
    pub fn new(event_type: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            event_type: event_type.into(),
            payload,
        }
    }

    /// Builds an event from a serializable value, using `event_type` as the
    /// logical type and the value's JSON encoding as the payload — pyfly's
    /// `payload = asdict(event)`.
    ///
    /// Returns [`CqrsError::Serialization`] when the value cannot be encoded.
    pub fn from_value<T: Serialize>(
        event_type: impl Into<String>,
        value: &T,
    ) -> Result<Self, CqrsError> {
        Ok(Self {
            event_type: event_type.into(),
            payload: serde_json::to_value(value)?,
        })
    }
}

/// A *result* type that exposes the domain events it produced — pyfly's
/// duck-typed `result.domain_events`. Implement it on a handler's result
/// (an aggregate / outcome) and publish them with
/// [`Bus::send_publishing`](crate::Bus::send_publishing).
///
/// Command-side events (pyfly's `command.domain_events`) live instead on the
/// [`Message::domain_events`](crate::Message::domain_events) hook, which
/// [`DomainEventMiddleware`] harvests automatically after dispatch.
///
/// The default returns no events, so a type that opts out behaves like a
/// pyfly object without a `domain_events` attribute.
pub trait DomainEvents {
    /// The domain events this value produced, in publication order.
    fn domain_events(&self) -> Vec<DomainEvent> {
        Vec::new()
    }
}

/// Strategy for handling a domain-event publishing failure — pyfly's
/// `EventFailureStrategy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EventFailureStrategy {
    /// Log failures and continue: the command still succeeds even if one or
    /// more events fail to publish (pyfly's default `LOG`).
    #[default]
    Log,
    /// Surface a [`CqrsError`] when any event fails to publish — pyfly's
    /// `RAISE`, which raises a `CommandProcessingException`.
    Raise,
}

/// Publishes domain events produced by command handlers — pyfly's
/// `CommandEventPublisher` port. Object-safe so the bus can hold an
/// `Arc<dyn CommandEventPublisher>`.
#[async_trait]
pub trait CommandEventPublisher: Send + Sync {
    /// Publishes one event to `destination` (or the publisher's own default
    /// when `None`) — pyfly's `publish(event, destination=...)`.
    async fn publish(
        &self,
        event: &DomainEvent,
        destination: Option<&str>,
    ) -> Result<(), CqrsError>;
}

/// A silent [`CommandEventPublisher`] — pyfly's `NoOpEventPublisher`, used
/// when no EDA integration is configured. Every `publish` is a successful
/// no-op.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoOpEventPublisher;

impl NoOpEventPublisher {
    /// Returns the no-op publisher.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CommandEventPublisher for NoOpEventPublisher {
    async fn publish(
        &self,
        event: &DomainEvent,
        _destination: Option<&str>,
    ) -> Result<(), CqrsError> {
        tracing::debug!(
            event_type = %event.event_type,
            "firefly/cqrs: NoOp publisher dropped domain event (no EDA configured)"
        );
        Ok(())
    }
}

/// A [`CommandEventPublisher`] backed by a [`firefly_eda::Publisher`] —
/// pyfly's `EdaCommandEventPublisher`.
///
/// Each [`DomainEvent`] is adapted to the canonical [`firefly_eda::Event`]
/// envelope (`event_type` → the event's `type`, JSON payload → the event
/// body) and published to the resolved destination topic. The destination is
/// the one passed to [`CommandEventPublisher::publish`] (pyfly's
/// `@publish_domain_event(destination=...)`), falling back to the configured
/// default (default [`DEFAULT_EVENT_DESTINATION`]).
pub struct EdaCommandEventPublisher {
    producer: Arc<dyn Publisher>,
    source: String,
    default_destination: String,
}

impl EdaCommandEventPublisher {
    /// Wraps `producer`, publishing to [`DEFAULT_EVENT_DESTINATION`] when a
    /// handler declares no destination — pyfly's
    /// `EdaCommandEventPublisher(producer)`.
    pub fn new(producer: Arc<dyn Publisher>) -> Self {
        Self {
            producer,
            source: "firefly-cqrs".to_string(),
            default_destination: DEFAULT_EVENT_DESTINATION.to_string(),
        }
    }

    /// Sets the default destination used when a handler declares none —
    /// pyfly's `default_destination` constructor argument.
    #[must_use]
    pub fn with_default_destination(mut self, destination: impl Into<String>) -> Self {
        self.default_destination = destination.into();
        self
    }

    /// Sets the `source` stamped on every published [`firefly_eda::Event`]
    /// (the logical producer / service name).
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

#[async_trait]
impl CommandEventPublisher for EdaCommandEventPublisher {
    async fn publish(
        &self,
        event: &DomainEvent,
        destination: Option<&str>,
    ) -> Result<(), CqrsError> {
        let topic = destination.unwrap_or(&self.default_destination);
        let payload = serde_json::to_vec(&event.payload)?;
        let envelope = Event::new(topic, &event.event_type, &self.source, Some(payload));
        self.producer
            .publish(envelope)
            .await
            .map_err(|err| CqrsError::EventPublish(err.to_string()))?;
        tracing::debug!(event_type = %event.event_type, topic, "firefly/cqrs: published domain event");
        Ok(())
    }
}

/// Publishes each event in `events` through `publisher`, applying `strategy`
/// to failures — the shared core of [`DomainEventMiddleware`] and
/// [`Bus::send_publishing`](crate::Bus::send_publishing), and the Rust
/// spelling of pyfly's `DefaultCommandBus._try_publish_events`.
///
/// Under [`EventFailureStrategy::Log`] every failure is logged and the call
/// returns `Ok`. Under [`EventFailureStrategy::Raise`] the first failure is
/// returned as a [`CqrsError::EventPublish`] after every event has been
/// attempted (matching pyfly, which tries them all then raises on the first).
pub async fn publish_domain_events(
    publisher: &dyn CommandEventPublisher,
    events: &[DomainEvent],
    destination: Option<&str>,
    strategy: EventFailureStrategy,
) -> Result<(), CqrsError> {
    let mut failures: Vec<(String, String)> = Vec::new();
    for event in events {
        if let Err(err) = publisher.publish(event, destination).await {
            tracing::error!(
                event_type = %event.event_type,
                error = %err,
                "firefly/cqrs: failed to publish domain event"
            );
            failures.push((event.event_type.clone(), err.to_string()));
        }
    }
    match strategy {
        EventFailureStrategy::Raise if !failures.is_empty() => {
            let (event_type, message) = &failures[0];
            Err(CqrsError::EventPublish(format!(
                "{} domain event(s) failed to publish; first failure ({event_type}): {message}",
                failures.len(),
            )))
        }
        _ if !failures.is_empty() => {
            tracing::error!(
                count = failures.len(),
                "firefly/cqrs: domain event(s) failed to publish"
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Bus [`Middleware`] that publishes a command's [`DomainEvents`] after a
/// successful dispatch — the Rust spelling of pyfly's
/// `@publish_domain_event` + `DefaultCommandBus._try_publish_events`
/// pipeline step.
///
/// The middleware harvests the *command's* events (pyfly's
/// `command.domain_events`). It only runs when dispatch succeeds: a failing
/// handler short-circuits before any event is published, matching pyfly
/// (events publish only after the handler returns). Result-side events
/// (pyfly's `result.domain_events`) are published via
/// [`Bus::send_publishing`](crate::Bus::send_publishing).
///
/// The publish destination defaults to the publisher's own default; attach a
/// per-message override with [`DomainEventMiddleware::with_destination`]
/// (pyfly's `@publish_domain_event(destination=...)`).
#[derive(Clone)]
pub struct DomainEventMiddleware {
    publisher: Arc<dyn CommandEventPublisher>,
    destination: Option<String>,
    strategy: EventFailureStrategy,
}

impl DomainEventMiddleware {
    /// Builds a middleware that publishes through `publisher` to its default
    /// destination, logging publish failures ([`EventFailureStrategy::Log`]).
    pub fn new(publisher: Arc<dyn CommandEventPublisher>) -> Self {
        Self {
            publisher,
            destination: None,
            strategy: EventFailureStrategy::default(),
        }
    }

    /// Overrides the publish destination for every command this middleware
    /// handles — pyfly's `@publish_domain_event(destination=...)`.
    #[must_use]
    pub fn with_destination(mut self, destination: impl Into<String>) -> Self {
        self.destination = Some(destination.into());
        self
    }

    /// Sets the [`EventFailureStrategy`] applied to publish failures
    /// (default [`EventFailureStrategy::Log`]).
    #[must_use]
    pub fn with_failure_strategy(mut self, strategy: EventFailureStrategy) -> Self {
        self.strategy = strategy;
        self
    }
}

impl Middleware for DomainEventMiddleware {
    fn wrap(&self, next: DynHandler) -> DynHandler {
        let publisher = Arc::clone(&self.publisher);
        let destination = self.destination.clone();
        let strategy = self.strategy;
        Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let next = Arc::clone(&next);
            let publisher = Arc::clone(&publisher);
            let destination = destination.clone();
            Box::pin(async move {
                // Capture the command's events before dispatch (the handler
                // consumes the message, but we read events off the envelope's
                // captured extractor, not the moved value).
                let events = env.domain_events();
                let result = next(env).await?;
                if !events.is_empty() {
                    publish_domain_events(
                        publisher.as_ref(),
                        &events,
                        destination.as_deref(),
                        strategy,
                    )
                    .await?;
                }
                Ok(result)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bus, Message};
    use std::sync::Mutex;

    #[derive(Clone, serde::Serialize)]
    struct PlaceOrder {
        id: String,
    }
    impl Message for PlaceOrder {
        fn domain_events(&self) -> Vec<DomainEvent> {
            vec![DomainEvent::new(
                "OrderPlaced",
                serde_json::json!({ "id": self.id }),
            )]
        }
    }

    #[derive(Clone, serde::Serialize)]
    struct Noisy;
    impl Message for Noisy {} // default: no events

    /// A recording publisher capturing (event_type, destination) pairs.
    #[derive(Default)]
    struct Recorder {
        published: Mutex<Vec<(String, Option<String>)>>,
        fail: bool,
    }

    #[async_trait]
    impl CommandEventPublisher for Recorder {
        async fn publish(
            &self,
            event: &DomainEvent,
            destination: Option<&str>,
        ) -> Result<(), CqrsError> {
            if self.fail {
                return Err(CqrsError::EventPublish("boom".into()));
            }
            self.published
                .lock()
                .unwrap()
                .push((event.event_type.clone(), destination.map(String::from)));
            Ok(())
        }
    }

    #[tokio::test]
    async fn middleware_publishes_command_events_after_dispatch() {
        let recorder = Arc::new(Recorder::default());
        let bus = Bus::new();
        bus.use_middleware(DomainEventMiddleware::new(recorder.clone()));
        bus.register(|_c: PlaceOrder| async move { Ok::<_, CqrsError>(()) });
        let _: () = bus.send(PlaceOrder { id: "o1".into() }).await.unwrap();
        let published = recorder.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, "OrderPlaced");
    }

    #[tokio::test]
    async fn middleware_uses_configured_destination() {
        let recorder = Arc::new(Recorder::default());
        let bus = Bus::new();
        bus.use_middleware(
            DomainEventMiddleware::new(recorder.clone()).with_destination("orders.events"),
        );
        bus.register(|_c: PlaceOrder| async move { Ok::<_, CqrsError>(()) });
        let _: () = bus.send(PlaceOrder { id: "o2".into() }).await.unwrap();
        assert_eq!(
            recorder.published.lock().unwrap()[0].1.as_deref(),
            Some("orders.events")
        );
    }

    #[tokio::test]
    async fn no_events_published_when_handler_fails() {
        let recorder = Arc::new(Recorder::default());
        let bus = Bus::new();
        bus.use_middleware(DomainEventMiddleware::new(recorder.clone()));
        bus.register(|_c: PlaceOrder| async move { Err::<(), _>(CqrsError::handler("declined")) });
        let res: Result<(), _> = bus.send(PlaceOrder { id: "o3".into() }).await;
        assert!(res.is_err());
        assert!(recorder.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn commands_without_events_publish_nothing() {
        let recorder = Arc::new(Recorder::default());
        let bus = Bus::new();
        bus.use_middleware(DomainEventMiddleware::new(recorder.clone()));
        bus.register(|_c: Noisy| async move { Ok::<_, CqrsError>(()) });
        let _: () = bus.send(Noisy).await.unwrap();
        assert!(recorder.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn raise_strategy_surfaces_publish_failure() {
        let recorder = Arc::new(Recorder {
            fail: true,
            ..Default::default()
        });
        let bus = Bus::new();
        bus.use_middleware(
            DomainEventMiddleware::new(recorder).with_failure_strategy(EventFailureStrategy::Raise),
        );
        bus.register(|_c: PlaceOrder| async move { Ok::<_, CqrsError>(()) });
        let res: Result<(), _> = bus.send(PlaceOrder { id: "o4".into() }).await;
        assert!(matches!(res, Err(CqrsError::EventPublish(_))));
    }

    #[tokio::test]
    async fn log_strategy_swallows_publish_failure() {
        let recorder = Arc::new(Recorder {
            fail: true,
            ..Default::default()
        });
        let bus = Bus::new();
        bus.use_middleware(DomainEventMiddleware::new(recorder)); // Log default
        bus.register(|_c: PlaceOrder| async move { Ok::<_, CqrsError>(()) });
        let res: Result<(), _> = bus.send(PlaceOrder { id: "o5".into() }).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn eda_publisher_forwards_to_broker() {
        use firefly_eda::{handler, InMemoryBroker};
        let broker = Arc::new(InMemoryBroker::new());
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        broker
            .subscribe(
                "cqrs.events",
                handler(move |ev| {
                    let seen = seen2.clone();
                    async move {
                        seen.lock().unwrap().push(ev.event_type);
                        Ok(())
                    }
                }),
            )
            .unwrap();

        let publisher = EdaCommandEventPublisher::new(broker.clone());
        publisher
            .publish(
                &DomainEvent::new("OrderPlaced", serde_json::json!({"id": "x"})),
                None,
            )
            .await
            .unwrap();
        // Allow the in-memory broker to deliver.
        tokio::task::yield_now().await;
        assert_eq!(*seen.lock().unwrap(), ["OrderPlaced"]);
    }
}
