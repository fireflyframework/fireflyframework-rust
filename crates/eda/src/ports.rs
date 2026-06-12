//! The `Publisher` / `Subscriber` / `Broker` ports and the delivery
//! [`Handler`] type.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use firefly_kernel::FireflyResult;
use futures::future::BoxFuture;

use crate::{EdaResult, Event};

/// The boxed future a [`Handler`] returns.
pub type HandlerFuture = BoxFuture<'static, FireflyResult<()>>;

/// The consumer-side delivery callback — the Rust spelling of Go's
/// `type Handler func(ctx context.Context, ev Event) error`.
///
/// Handlers are reference-counted closures so a single subscription can
/// be invoked for every delivery; build one ergonomically with
/// [`handler`].
pub type Handler = Arc<dyn Fn(Event) -> HandlerFuture + Send + Sync>;

/// Wraps an async closure as a [`Handler`], boxing the returned future.
///
/// ```
/// use firefly_eda::{handler, Event};
///
/// let h = handler(|ev: Event| async move {
///     assert_eq!(ev.topic, "orders.created");
///     Ok(())
/// });
/// ```
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Event) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = FireflyResult<()>> + Send + 'static,
{
    Arc::new(move |ev| Box::pin(f(ev)))
}

/// Publishes events to a broker. Object-safe; the async methods box
/// their futures via [`async_trait`].
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Publishes `ev` to its topic.
    async fn publish(&self, ev: Event) -> EdaResult<()>;
    /// Releases the publisher; subsequent operations fail with
    /// [`EdaError::Closed`](crate::EdaError::Closed).
    async fn close(&self) -> EdaResult<()>;
}

/// Subscribes a [`Handler`] to a topic. Object-safe; the async methods
/// box their futures via [`async_trait`].
#[async_trait]
pub trait Subscriber: Send + Sync {
    /// Registers `h` for every event published to `topic`.
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()>;
    /// Releases the subscriber; subsequent operations fail with
    /// [`EdaError::Closed`](crate::EdaError::Closed).
    async fn close(&self) -> EdaResult<()>;
}

/// Exposes both surfaces for adapters that combine them (Kafka,
/// RabbitMQ, the in-memory broker) — Go's `Broker` interface embedding
/// `Publisher` and `Subscriber`.
///
/// Blanket-implemented for every type that is both a [`Publisher`] and
/// a [`Subscriber`]. Note that on a `dyn Broker` the two inherited
/// `close` methods collide; disambiguate with
/// `Publisher::close(&broker)` (both release the whole broker).
pub trait Broker: Publisher + Subscriber {}

impl<T: Publisher + Subscriber + ?Sized> Broker for T {}
