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

//! The **reactive (Reactor / WebFlux-style) surface** over the
//! [`InMemoryBroker`](crate::InMemoryBroker).
//!
//! This module is strictly *additive*: it layers a
//! [`firefly_reactive`] `Flux` / `Mono` façade over the existing
//! [`Publisher`](crate::Publisher) / [`Subscriber`](crate::Subscriber)
//! mechanism without changing a single existing signature or wire
//! format. It is the EDA analog of consuming a Spring `KafkaListener`
//! topic as a `Flux<Event>` (Reactor Kafka's `KafkaReceiver.receive()`)
//! and of publishing through a `Mono` (`reactiveKafkaTemplate.send(..)`).
//!
//! Two entry points are added to [`InMemoryBroker`]:
//!
//! - [`subscribe_reactive`](InMemoryBroker::subscribe_reactive) — turns a
//!   topic subscription into a [`Flux<Event>`] that emits every event
//!   delivered to that topic, backed by a *bounded* channel with
//!   on-backpressure-drop semantics (a slow downstream never stalls or
//!   fails the publisher — the broker's existing "a dropped receiver
//!   never fails publishers" contract, extended to the reactive surface).
//! - [`publish_mono`](InMemoryBroker::publish_mono) — the reactive,
//!   *cold* publish helper: nothing happens until the returned
//!   [`Mono<()>`] is subscribed/awaited, the Reactor analog of
//!   `template.send(..)` returning a `Mono<Void>`.

use std::sync::Arc;

use firefly_reactive::{Flux, Mono};
use tokio::sync::mpsc;

use crate::{handler, Event, InMemoryBroker};

/// The default backpressure window for
/// [`subscribe_reactive`](InMemoryBroker::subscribe_reactive): how many
/// undelivered events the bounded channel holds before the broker starts
/// dropping the newest events for a slow subscriber.
///
/// Reactor's `onBackpressureBuffer` default prefetch is 256; we match it
/// so the reactive EDA surface feels familiar to a WebFlux user.
pub const DEFAULT_REACTIVE_BUFFER: usize = 256;

impl InMemoryBroker {
    /// Subscribes to `topic` and returns a [`Flux<Event>`] that emits
    /// every event delivered to that topic.
    ///
    /// This is the reactive twin of
    /// [`subscribe_channel`](InMemoryBroker::subscribe_channel): instead
    /// of handing back a raw `mpsc::Receiver`, it wraps the receiver in a
    /// `Flux` so it composes with the whole Reactor operator set
    /// (`take`, `filter`, `map`, `collect_list`, …). It is the EDA analog
    /// of Reactor Kafka's `KafkaReceiver.receive()` yielding a
    /// `Flux<ReceiverRecord>`.
    ///
    /// **Backpressure.** Deliveries are buffered through a *bounded*
    /// channel of [`DEFAULT_REACTIVE_BUFFER`] events. When the downstream
    /// `Flux` consumer falls behind and the buffer fills, further events
    /// are *dropped* for this subscription (newest-dropped,
    /// `onBackpressureDrop` semantics) rather than blocking the
    /// publisher's task — preserving the broker's invariant that a slow
    /// or gone consumer never fails publishers. Use
    /// [`subscribe_reactive_with_buffer`](InMemoryBroker::subscribe_reactive_with_buffer)
    /// to size the window explicitly.
    ///
    /// **Termination.** The `Flux` completes when every sender for the
    /// subscription is dropped — which happens when the broker is
    /// [`close`](InMemoryBroker::close)d (all subscriptions, and thus the
    /// retained handler closures holding the sender, are cleared). A
    /// `Flux` whose downstream is dropped simply stops draining; the
    /// broker silently discards further deliveries.
    ///
    /// `topic` may be a glob pattern, exactly as
    /// [`subscribe`](InMemoryBroker::subscribe).
    ///
    /// ```
    /// use firefly_eda::{Event, InMemoryBroker};
    ///
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
    /// let broker = InMemoryBroker::new();
    /// let flux = broker.subscribe_reactive("orders.*").unwrap();
    ///
    /// broker
    ///     .publish(Event::new("orders.created", "OrderCreated", "svc", None))
    ///     .await
    ///     .unwrap();
    /// broker.close().unwrap(); // drops the sender → terminates the Flux
    ///
    /// let events = flux.collect_list().block().await.unwrap().unwrap();
    /// assert_eq!(events.len(), 1);
    /// assert_eq!(events[0].topic, "orders.created");
    /// # });
    /// ```
    pub fn subscribe_reactive(&self, topic: impl Into<String>) -> crate::EdaResult<Flux<Event>> {
        self.subscribe_reactive_with_buffer(topic, DEFAULT_REACTIVE_BUFFER)
    }

    /// [`subscribe_reactive`](InMemoryBroker::subscribe_reactive) with an
    /// explicit backpressure window `buffer` (clamped to at least 1).
    ///
    /// A smaller `buffer` drops sooner under a slow consumer; a larger
    /// one tolerates longer bursts before dropping. This is the knob
    /// behind Reactor's `onBackpressureBuffer(maxSize)`.
    pub fn subscribe_reactive_with_buffer(
        &self,
        topic: impl Into<String>,
        buffer: usize,
    ) -> crate::EdaResult<Flux<Event>> {
        // A bounded channel is the backpressure window. `try_send` from
        // the delivery handler means a full buffer drops the newest event
        // (onBackpressureDrop) instead of stalling the publisher — the
        // reactive equivalent of `subscribe_channel`'s "dropped receiver
        // never fails publishers" guarantee.
        let (tx, rx) = mpsc::channel::<Event>(buffer.max(1));
        self.subscribe(
            topic,
            handler(move |ev: Event| {
                let tx = tx.clone();
                async move {
                    // Drop the event if the consumer is slow or gone; a
                    // publisher is never blocked or failed by a reactive
                    // subscriber.
                    let _ = tx.try_send(ev);
                    Ok(())
                }
            }),
        )?;

        // Drain the receiver into a Flux. When the broker is closed the
        // retained handler (and its `tx`) is dropped, the channel closes,
        // and the stream ends — terminating the Flux.
        Ok(Flux::from_value_stream(ReceiverStream::new(rx)))
    }

    /// Publishes `ev` reactively, returning a *cold* [`Mono<()>`] that
    /// performs the publish only when subscribed/awaited.
    ///
    /// This is the Reactor analog of a reactive `KafkaTemplate.send(..)`
    /// returning a `Mono<Void>`: building the `Mono` does nothing; the
    /// publish fan-out runs when the `Mono` is driven (`.block().await`,
    /// `.subscribe(..)`, or composed into a larger pipeline). The success
    /// signal carries `()`; a handler error or a closed broker surfaces
    /// as the `Mono`'s error signal (an
    /// [`EdaError`](crate::EdaError)-derived [`FireflyError`]).
    ///
    /// It hangs off `Arc<InMemoryBroker>` so the returned `Mono` can own
    /// its broker handle and outlive the call site — drop it into any
    /// reactive pipeline.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use firefly_eda::{Event, InMemoryBroker};
    ///
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
    /// let broker = Arc::new(InMemoryBroker::new());
    /// let flux = broker.subscribe_reactive("orders.created").unwrap();
    ///
    /// broker
    ///     .publish_mono(Event::new("orders.created", "OrderCreated", "svc", None))
    ///     .block()
    ///     .await
    ///     .unwrap();
    /// broker.close().unwrap();
    ///
    /// let n = flux.count().block().await.unwrap();
    /// assert_eq!(n, Some(1));
    /// # });
    /// ```
    pub fn publish_mono(self: &Arc<Self>, ev: Event) -> Mono<()> {
        let broker = Arc::clone(self);
        Mono::from_result_future(async move {
            broker
                .publish(ev)
                .await
                .map_err(firefly_kernel::FireflyError::from)
        })
    }
}

/// Adapts a [`tokio::sync::mpsc::Receiver`] to a [`futures::Stream`] so
/// it can feed [`Flux::from_value_stream`].
///
/// A tiny local shim (rather than a `tokio-stream` dependency) keeping
/// the reactive surface's dependency footprint to the workspace deps the
/// crate already declares.
struct ReceiverStream<T> {
    rx: mpsc::Receiver<T>,
}

impl<T> ReceiverStream<T> {
    fn new(rx: mpsc::Receiver<T>) -> Self {
        Self { rx }
    }
}

impl<T> futures::Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}
