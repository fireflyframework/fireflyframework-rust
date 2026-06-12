//! Programmatic, push-style emission into a [`Flux`](crate::Flux) — the
//! Rust analog of Reactor's `FluxSink` / `Flux.create`.
//!
//! Where most factories pull from an existing source, [`Flux::create`](crate::Flux::create)
//! hands you a [`FluxSink`] you can drive imperatively from a callback,
//! a spawned task, or a bridge to a non-reactive callback API. Emissions
//! are buffered through a bounded channel, so a fast producer is held
//! back by a slow consumer — natural backpressure.
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
    tx: mpsc::Sender<Result<T, FireflyError>>,
}

impl<T> FluxSink<T>
where
    T: Send + 'static,
{
    /// Creates a sink paired with the receiver the
    /// [`Flux`](crate::Flux) drains. `buffer` is the channel capacity
    /// (the backpressure window); it is clamped to at least 1.
    pub(crate) fn channel(buffer: usize) -> (Self, mpsc::Receiver<Result<T, FireflyError>>) {
        let (tx, rx) = mpsc::channel(buffer.max(1));
        (Self { tx }, rx)
    }

    /// Emits the next value. Returns `true` if it was accepted, `false`
    /// if the downstream has already gone away (cancelled or
    /// terminated), in which case the producer should stop.
    pub fn next(&self, value: T) -> bool {
        self.tx.try_send(Ok(value)).is_ok()
    }

    /// Emits `value`, awaiting capacity if the bounded buffer is full.
    ///
    /// Unlike [`next`](FluxSink::next), which drops the value when the
    /// buffer is full, this applies real backpressure: it suspends the
    /// producer until the consumer drains a slot. Returns `false` once
    /// the downstream is gone.
    pub async fn send(&self, value: T) -> bool {
        self.tx.send(Ok(value)).await.is_ok()
    }

    /// Terminates the stream with an error. No further items are
    /// emitted.
    pub fn error(&self, err: FireflyError) {
        let _ = self.tx.try_send(Err(err));
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
        self.tx.is_closed()
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
