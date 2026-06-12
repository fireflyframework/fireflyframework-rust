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

//! Programmatic, push-style emission into a [`Flux`](crate::Flux) — the
//! Rust analog of Reactor's `FluxSink` / `Flux.create`.
//!
//! Where most factories pull from an existing source, [`Flux::create`](crate::Flux::create)
//! hands you a [`FluxSink`] you can drive imperatively from a callback,
//! a spawned task, or a bridge to a non-reactive callback API.
//!
//! [`Flux::create`](crate::Flux::create) buffers through an **unbounded**
//! channel, matching Reactor's default `OverflowStrategy.BUFFER`: a
//! synchronous burst producer that pushes every item (and a terminal
//! `error`) before the stream is ever polled never loses an item or the
//! terminal signal. [`Flux::create_with_buffer`](crate::Flux::create_with_buffer)
//! opts into a *bounded* buffer where [`send`](FluxSink::send) applies
//! real backpressure by awaiting a free slot.
//!
//! ```
//! use firefly_reactive::Flux;
//!
//! # async fn ex() {
//! let flux = Flux::create(|sink| {
//!     for i in 1..=3 {
//!         sink.next(i);
//!     }
//!     sink.complete();
//! });
//! let out = flux.collect_list().block().await.unwrap();
//! assert_eq!(out, Some(vec![1, 2, 3]));
//! # }
//! ```

use firefly_kernel::FireflyError;
use tokio::sync::mpsc;

/// The receiving half a [`Flux`](crate::Flux) drains, matched to the
/// [`FluxSink`]'s sender variant.
pub(crate) enum SinkReceiver<T> {
    Bounded(mpsc::Receiver<Result<T, FireflyError>>),
    Unbounded(mpsc::UnboundedReceiver<Result<T, FireflyError>>),
}

impl<T> SinkReceiver<T> {
    /// Awaits the next buffered signal, or `None` once every sink clone
    /// has been dropped and the buffer is drained.
    pub(crate) async fn recv(&mut self) -> Option<Result<T, FireflyError>> {
        match self {
            SinkReceiver::Bounded(rx) => rx.recv().await,
            SinkReceiver::Unbounded(rx) => rx.recv().await,
        }
    }
}

/// The sending half — either a bounded channel (real backpressure via
/// [`send`](FluxSink::send)) or an unbounded one (Reactor's default
/// never-drop `BUFFER`).
enum SinkSender<T> {
    Bounded(mpsc::Sender<Result<T, FireflyError>>),
    Unbounded(mpsc::UnboundedSender<Result<T, FireflyError>>),
}

impl<T> Clone for SinkSender<T> {
    fn clone(&self) -> Self {
        match self {
            SinkSender::Bounded(tx) => SinkSender::Bounded(tx.clone()),
            SinkSender::Unbounded(tx) => SinkSender::Unbounded(tx.clone()),
        }
    }
}

/// The push handle given to the [`Flux::create`](crate::Flux::create)
/// callback.
///
/// Call [`next`](FluxSink::next) to emit a value, [`error`](FluxSink::error)
/// to terminate with a failure, and [`complete`](FluxSink::complete) to
/// terminate normally. After a terminal signal further emissions are
/// silently ignored, matching Reactor's `FluxSink` contract.
///
/// The sink is cloneable: hand clones to multiple producers and the
/// stream completes when the last clone is dropped (or when any clone
/// signals a terminal event).
#[derive(Clone)]
pub struct FluxSink<T> {
    tx: SinkSender<T>,
}

impl<T> FluxSink<T>
where
    T: Send + 'static,
{
    /// Creates an **unbounded** sink (Reactor's default
    /// `OverflowStrategy.BUFFER`): [`next`](FluxSink::next) and
    /// [`error`](FluxSink::error) never drop, even when the producer runs
    /// a synchronous burst before the stream is polled.
    pub(crate) fn unbounded() -> (Self, SinkReceiver<T>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx: SinkSender::Unbounded(tx),
            },
            SinkReceiver::Unbounded(rx),
        )
    }

    /// Creates a sink paired with the receiver the
    /// [`Flux`](crate::Flux) drains. `buffer` is the channel capacity
    /// (the backpressure window for [`send`](FluxSink::send)); it is
    /// clamped to at least 1.
    pub(crate) fn channel(buffer: usize) -> (Self, SinkReceiver<T>) {
        let (tx, rx) = mpsc::channel(buffer.max(1));
        (
            Self {
                tx: SinkSender::Bounded(tx),
            },
            SinkReceiver::Bounded(rx),
        )
    }

    /// Emits the next value. Returns `true` if it was accepted, `false`
    /// if the downstream has already gone away (cancelled or
    /// terminated), in which case the producer should stop.
    ///
    /// On an unbounded sink ([`Flux::create`](crate::Flux::create)) this
    /// never drops. On a bounded sink
    /// ([`Flux::create_with_buffer`](crate::Flux::create_with_buffer))
    /// it returns `false` if the buffer is currently full — use
    /// [`send`](FluxSink::send) there to await capacity instead.
    pub fn next(&self, value: T) -> bool {
        match &self.tx {
            SinkSender::Bounded(tx) => tx.try_send(Ok(value)).is_ok(),
            SinkSender::Unbounded(tx) => tx.send(Ok(value)).is_ok(),
        }
    }

    /// Emits `value`, awaiting capacity if a bounded buffer is full.
    ///
    /// On a bounded sink this applies real backpressure: it suspends the
    /// producer until the consumer drains a slot. On an unbounded sink it
    /// accepts immediately. Returns `false` once the downstream is gone.
    pub async fn send(&self, value: T) -> bool {
        match &self.tx {
            SinkSender::Bounded(tx) => tx.send(Ok(value)).await.is_ok(),
            SinkSender::Unbounded(tx) => tx.send(Ok(value)).is_ok(),
        }
    }

    /// Terminates the stream with an error. No further items are
    /// emitted. The terminal error is never dropped (the unbounded
    /// default always accepts it; a bounded sink falls back to a
    /// blocking send so the error is not lost when the buffer is full).
    pub fn error(&self, err: FireflyError) {
        match &self.tx {
            SinkSender::Bounded(tx) => match tx.try_send(Err(err)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(item)) => {
                    // Buffer full but downstream alive: do not lose the
                    // terminal signal. Hand it to a detached task that
                    // awaits a free slot.
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(item).await;
                    });
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            },
            SinkSender::Unbounded(tx) => {
                let _ = tx.send(Err(err));
            }
        }
    }

    /// Terminates the stream normally. Equivalent to dropping the last
    /// sink clone; provided for symmetry with Reactor's
    /// `FluxSink.complete()`.
    pub fn complete(self) {
        drop(self);
    }

    /// Reports whether the downstream is still listening. Once this
    /// returns `true` the producer should stop emitting.
    pub fn is_cancelled(&self) -> bool {
        match &self.tx {
            SinkSender::Bounded(tx) => tx.is_closed(),
            SinkSender::Unbounded(tx) => tx.is_closed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn next_then_complete() {
        let (sink, mut rx) = FluxSink::channel(8);
        assert!(sink.next(1));
        assert!(sink.next(2));
        sink.complete();
        assert!(matches!(rx.recv().await, Some(Ok(1))));
        assert!(matches!(rx.recv().await, Some(Ok(2))));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn error_terminates() {
        let (sink, mut rx) = FluxSink::<i32>::channel(8);
        sink.error(FireflyError::internal("x"));
        assert!(matches!(rx.recv().await, Some(Err(_))));
    }

    #[tokio::test]
    async fn cancelled_when_receiver_dropped() {
        let (sink, rx) = FluxSink::<i32>::channel(8);
        drop(rx);
        assert!(sink.is_cancelled());
        assert!(!sink.next(1));
    }
}
