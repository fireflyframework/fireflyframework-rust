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
///
/// The `topic` passed to [`subscribe`](Subscriber::subscribe) may be a
/// glob *pattern* (`*`, `?`, `[..]`, `{a,b}` alternation): an event is
/// delivered to a subscription when the event's `topic` matches the
/// pattern. An exact string with no glob metacharacters matches only
/// itself, so existing exact-topic subscriptions behave unchanged. This
/// mirrors pyfly's `fnmatch`-based event-type pattern dispatch.
#[async_trait]
pub trait Subscriber: Send + Sync {
    /// Registers `h` for every event whose topic matches `topic` (an
    /// exact name or a glob pattern).
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()>;

    /// Registers `h` as a member of consumer `group` for `topic`. Within
    /// a group, each matching event is delivered to exactly **one**
    /// member (round-robin across the group's handlers); subscriptions
    /// in different groups — and ungrouped subscriptions — each receive
    /// their own copy. This is pyfly's `subscribe(topic, handler,
    /// group=…)` competing-consumer semantics.
    ///
    /// The default implementation ignores the group and delegates to
    /// [`subscribe`](Subscriber::subscribe) — correct for transports
    /// (Kafka, RabbitMQ) whose broker enforces group delivery natively.
    /// In-process brokers such as [`InMemoryBroker`](crate::InMemoryBroker)
    /// override it to implement round-robin themselves.
    async fn subscribe_group(&self, topic: &str, _group: &str, h: Handler) -> EdaResult<()> {
        self.subscribe(topic, h).await
    }

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
