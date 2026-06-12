//! The in-process fan-out broker.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{handler, EdaError, EdaResult, Event, Handler, Publisher, Subscriber};

/// The canonical in-process [`Broker`](crate::Broker). Each topic has a
/// fan-out list of handlers; [`InMemoryBroker::publish`] runs every
/// handler sequentially on the publisher's task — the Rust analog of
/// the Go broker invoking handlers synchronously in the publisher's
/// goroutine.
///
/// Suitable for tests, single-binary services, and the default starter
/// configuration that does not opt into Kafka or RabbitMQ.
#[derive(Default)]
pub struct InMemoryBroker {
    inner: RwLock<Inner>,
}

#[derive(Default)]
struct Inner {
    handlers: HashMap<String, Vec<Handler>>,
    closed: bool,
}

impl InMemoryBroker {
    /// Returns an empty in-memory broker — Go's `NewInMemory()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `ev` to every handler subscribed to its topic. The
    /// first handler error short-circuits dispatch to remaining
    /// handlers and is returned to the caller (wrapped transparently in
    /// [`EdaError::Handler`]) — matching the Java/.NET/Go semantics.
    pub async fn publish(&self, ev: Event) -> EdaResult<()> {
        let snapshot = {
            let inner = self.inner.read().expect("firefly/eda: lock poisoned");
            if inner.closed {
                return Err(EdaError::Closed);
            }
            inner.handlers.get(&ev.topic).cloned().unwrap_or_default()
        };
        for h in snapshot {
            h(ev.clone()).await?;
        }
        Ok(())
    }

    /// Registers `h` for every event published to `topic`.
    pub fn subscribe(&self, topic: impl Into<String>, h: Handler) -> EdaResult<()> {
        let mut inner = self.inner.write().expect("firefly/eda: lock poisoned");
        if inner.closed {
            return Err(EdaError::Closed);
        }
        inner.handlers.entry(topic.into()).or_default().push(h);
        Ok(())
    }

    /// Rust-specific convenience: subscribes a channel to `topic` and
    /// returns its receiving half. Every published event is forwarded
    /// into the channel; dropping the receiver simply discards further
    /// deliveries without failing publishers.
    pub fn subscribe_channel(
        &self,
        topic: impl Into<String>,
    ) -> EdaResult<mpsc::UnboundedReceiver<Event>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribe(
            topic,
            handler(move |ev: Event| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(ev); // receiver gone — drop silently
                    Ok(())
                }
            }),
        )?;
        Ok(rx)
    }

    /// Marks the broker as closed and drops all subscriptions;
    /// subsequent publish/subscribe calls return [`EdaError::Closed`].
    /// Idempotent, like Go's `Close() error` returning `nil`.
    pub fn close(&self) -> EdaResult<()> {
        let mut inner = self.inner.write().expect("firefly/eda: lock poisoned");
        inner.closed = true;
        inner.handlers.clear();
        Ok(())
    }
}

#[async_trait]
impl Publisher for InMemoryBroker {
    async fn publish(&self, ev: Event) -> EdaResult<()> {
        InMemoryBroker::publish(self, ev).await
    }

    async fn close(&self) -> EdaResult<()> {
        InMemoryBroker::close(self)
    }
}

#[async_trait]
impl Subscriber for InMemoryBroker {
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()> {
        InMemoryBroker::subscribe(self, topic, h)
    }

    async fn close(&self) -> EdaResult<()> {
        InMemoryBroker::close(self)
    }
}
