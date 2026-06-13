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

/// Ergonomic publish helpers layered over the object-safe [`Publisher`]
/// port. Blanket-implemented for every `Publisher`, including
/// `dyn Publisher` / `Arc<dyn Publisher>`, so it is available without
/// extra wiring — the Rust convenience for the common "publish this
/// payload to this topic" call that otherwise spells out
/// [`Event::new`] every time.
///
/// ```
/// use firefly_eda::{InMemoryBroker, Publisher, PublisherExt};
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let broker = InMemoryBroker::new();
/// // One call instead of `Event::new(..)` + `publish(..)`.
/// broker
///     .publish_bytes("orders.created", "OrderCreated", "orders-svc", Some(br#"{"id":"o1"}"#.to_vec()))
///     .await
///     .unwrap();
/// broker.close().unwrap();
/// # });
/// ```
#[async_trait]
pub trait PublisherExt: Publisher {
    /// Builds an [`Event`] from `topic` / `event_type` / `source` /
    /// `payload` (via [`Event::new`], so the correlation id is stamped
    /// from the ambient scope) and publishes it. The raw-bytes
    /// convenience over [`Publisher::publish`].
    async fn publish_bytes(
        &self,
        topic: impl Into<String> + Send,
        event_type: impl Into<String> + Send,
        source: impl Into<String> + Send,
        payload: Option<Vec<u8>>,
    ) -> EdaResult<()> {
        self.publish(Event::new(topic, event_type, source, payload))
            .await
    }
}

#[async_trait]
impl<T: Publisher + ?Sized> PublisherExt for T {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::InMemoryBroker;

    #[tokio::test]
    async fn publish_bytes_builds_and_delivers_an_event() {
        let broker = InMemoryBroker::new();
        let seen = Arc::new(AtomicU32::new(0));
        let s = Arc::clone(&seen);
        broker
            .subscribe(
                "orders.created",
                handler(move |ev: Event| {
                    let s = Arc::clone(&s);
                    async move {
                        assert_eq!(ev.event_type, "OrderCreated");
                        assert_eq!(ev.source, "orders-svc");
                        assert_eq!(ev.payload.as_deref(), Some(&b"{\"id\":\"o1\"}"[..]));
                        s.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            )
            .unwrap();

        broker
            .publish_bytes(
                "orders.created",
                "OrderCreated",
                "orders-svc",
                Some(br#"{"id":"o1"}"#.to_vec()),
            )
            .await
            .unwrap();

        assert_eq!(seen.load(Ordering::SeqCst), 1);
        // Inherent (synchronous) close on the concrete broker.
        broker.close().unwrap();
    }

    #[tokio::test]
    async fn publish_bytes_available_through_arc_dyn_publisher() {
        let broker: Arc<dyn Publisher> = Arc::new(InMemoryBroker::new());
        // The extension is reachable on a trait object too.
        broker.publish_bytes("t", "T", "s", None).await.unwrap();
    }
}
