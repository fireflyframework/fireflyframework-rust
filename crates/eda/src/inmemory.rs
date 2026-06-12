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

//! The in-process fan-out broker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use tokio::sync::mpsc;

use crate::{handler, EdaError, EdaResult, Event, Handler, Publisher, Subscriber};

/// The canonical in-process [`Broker`](crate::Broker). Each subscription
/// carries a glob topic pattern, an optional consumer group, and a
/// handler; [`InMemoryBroker::publish`] runs every *matching* handler
/// sequentially on the publisher's task — the Rust analog of the Go
/// broker invoking handlers synchronously in the publisher's goroutine.
///
/// Two dispatch modes coexist (pyfly's `InMemoryEventBus` /
/// `InMemoryMessageBroker` semantics):
///
/// - **Ungrouped subscriptions** (via [`subscribe`](InMemoryBroker::subscribe))
///   each receive their own copy of every matching event — fan-out.
/// - **Grouped subscriptions** (via
///   [`subscribe_group`](InMemoryBroker::subscribe_group)) compete:
///   within a group, each matching event is delivered to exactly one
///   member, chosen round-robin via a per-group [`AtomicUsize`] cursor.
///
/// Topic strings may be **glob patterns** (`*`, `?`, `[..]`, `{a,b}`):
/// `orders.*` matches `orders.created`. A pattern with no glob
/// metacharacters matches only its exact literal, so existing
/// exact-topic subscriptions are unaffected.
///
/// Suitable for tests, single-binary services, and the default starter
/// configuration that does not opt into Kafka or RabbitMQ.
#[derive(Default)]
pub struct InMemoryBroker {
    inner: RwLock<Inner>,
}

#[derive(Default)]
struct Inner {
    subscriptions: Vec<Subscription>,
    /// Round-robin cursor keyed by the `(event topic, consumer group)`
    /// pair. pyfly's `InMemoryMessageBroker` keys its cursor by the same
    /// `(topic, group)` pair (`messaging/adapters/memory.py`), so a group
    /// that spans multiple topics keeps an *independent* cursor per topic.
    /// Sharing one cursor across topics — when the per-event matching set
    /// differs in size between topics — perturbs the modulo base and
    /// starves members of one topic's set. Cursors are created lazily on
    /// first dispatch for a given pair (so they survive new members joining
    /// without resetting fairness mid-stream) and kept in an `Arc` so the
    /// chosen cursor can be cloned out of the read lock and advanced
    /// without holding the write guard during dispatch.
    group_cursors: HashMap<(String, String), Arc<AtomicUsize>>,
    closed: bool,
}

struct Subscription {
    matcher: GlobMatcher,
    group: Option<String>,
    handler: Handler,
}

impl InMemoryBroker {
    /// Returns an empty in-memory broker — Go's `NewInMemory()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `ev` to every handler whose topic pattern matches
    /// `ev.topic`. Ungrouped subscriptions each fire; for each consumer
    /// group with at least one matching handler, exactly one handler
    /// fires (round-robin). The first handler error short-circuits
    /// dispatch to remaining handlers and is returned to the caller
    /// (wrapped transparently in [`EdaError::Handler`]) — matching the
    /// Java/.NET/Go semantics.
    pub async fn publish(&self, ev: Event) -> EdaResult<()> {
        // Snapshot the matching handlers under the read lock, choosing
        // the round-robin winner per group, then dispatch outside the
        // lock so handlers may re-enter the broker without deadlocking.
        let to_invoke = self.select_handlers(&ev)?;
        for h in to_invoke {
            h(ev.clone()).await?;
        }
        Ok(())
    }

    /// Builds the ordered list of handlers that must run for `ev`:
    /// every matching ungrouped handler in subscription order, plus one
    /// round-robin-selected handler per group with matching members.
    fn select_handlers(&self, ev: &Event) -> EdaResult<Vec<Handler>> {
        // Round-robin cursors are keyed by the `(event topic, group)` pair,
        // which is only known once we have the event topic, so we may need
        // to lazily create cursors. Take the write lock unconditionally —
        // dispatch under the in-memory broker is cheap and this keeps the
        // cursor lookup/creation atomic without a read→write upgrade dance.
        let inner = &mut *self.inner.write().expect("firefly/eda: lock poisoned");
        if inner.closed {
            return Err(EdaError::Closed);
        }

        let mut to_invoke: Vec<Handler> = Vec::new();
        // Per-group matching handlers, preserving subscription order so
        // the round-robin sequence is deterministic. The group name is
        // cloned (owned) so the borrow of `inner.subscriptions` ends before
        // we mutate `inner.group_cursors` below.
        let mut group_matches: HashMap<String, Vec<Handler>> = HashMap::new();

        for sub in &inner.subscriptions {
            if !sub.matcher.is_match(ev.topic.as_str()) {
                continue;
            }
            match &sub.group {
                None => to_invoke.push(Arc::clone(&sub.handler)),
                Some(group) => group_matches
                    .entry(group.clone())
                    .or_default()
                    .push(Arc::clone(&sub.handler)),
            }
        }

        // For each group with matching members, advance its per-`(topic,
        // group)` cursor — independent across topics, matching pyfly.
        for (group, handlers) in group_matches {
            let cursor = inner
                .group_cursors
                .entry((ev.topic.clone(), group))
                .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
            let idx = cursor.fetch_add(1, Ordering::Relaxed) % handlers.len();
            to_invoke.push(handlers[idx].clone());
        }

        Ok(to_invoke)
    }

    /// Registers `h` for every event whose topic matches `topic` (an
    /// exact name or a glob pattern). Fan-out: every ungrouped handler
    /// matching a published event receives its own copy.
    ///
    /// Returns [`EdaError::Handler`] wrapping a `400` if `topic` is not a
    /// valid glob pattern.
    pub fn subscribe(&self, topic: impl Into<String>, h: Handler) -> EdaResult<()> {
        self.add_subscription(topic.into(), None, h)
    }

    /// Registers `h` as a member of consumer `group` for `topic`. Within
    /// the group, each matching event is delivered to exactly one member
    /// (round-robin) — pyfly's competing-consumer subscription.
    pub fn subscribe_group(
        &self,
        topic: impl Into<String>,
        group: impl Into<String>,
        h: Handler,
    ) -> EdaResult<()> {
        self.add_subscription(topic.into(), Some(group.into()), h)
    }

    fn add_subscription(&self, topic: String, group: Option<String>, h: Handler) -> EdaResult<()> {
        let matcher = Glob::new(&topic)
            .map_err(|e| {
                EdaError::Handler(firefly_kernel::FireflyError::bad_request(format!(
                    "firefly/eda: invalid topic pattern {topic:?}: {e}"
                )))
            })?
            .compile_matcher();

        let mut inner = self.inner.write().expect("firefly/eda: lock poisoned");
        if inner.closed {
            return Err(EdaError::Closed);
        }
        // Cursors are keyed by `(event topic, group)` and created lazily on
        // first dispatch, since the concrete event topic (not the glob
        // subscription pattern) is only known at publish time.
        inner.subscriptions.push(Subscription {
            matcher,
            group,
            handler: h,
        });
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
        inner.subscriptions.clear();
        inner.group_cursors.clear();
        Ok(())
    }

    /// Whether the broker has been [`close`](InMemoryBroker::close)d. The
    /// in-memory broker has no other failure mode, so this is the liveness
    /// signal the [`EventPublisherHealthIndicator`](crate::EventPublisherHealthIndicator)
    /// surfaces.
    pub fn is_closed(&self) -> bool {
        self.inner
            .read()
            .expect("firefly/eda: lock poisoned")
            .closed
    }
}

#[async_trait]
impl crate::BrokerHealth for InMemoryBroker {
    /// The in-memory broker is live unless it has been closed; a closed
    /// broker pings with [`EdaError::Closed`].
    async fn ping(&self) -> EdaResult<()> {
        if self.is_closed() {
            Err(EdaError::Closed)
        } else {
            Ok(())
        }
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

    async fn subscribe_group(&self, topic: &str, group: &str, h: Handler) -> EdaResult<()> {
        InMemoryBroker::subscribe_group(self, topic, group, h)
    }

    async fn close(&self) -> EdaResult<()> {
        InMemoryBroker::close(self)
    }
}
