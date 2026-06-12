//! Semaphore-based concurrency bulkhead.

use std::future::Future;

use tokio::sync::Semaphore;

use crate::error::ResilienceError;

/// `Bulkhead` caps concurrent calls — analogous to a thread-pool isolation
/// boundary in JVM frameworks. Where the Go port uses a buffered channel as
/// the semaphore, the Rust port uses [`tokio::sync::Semaphore`].
#[derive(Debug)]
pub struct Bulkhead {
    sem: Semaphore,
    max_concurrent: usize,
}

impl Bulkhead {
    /// Returns a bulkhead allowing up to `max_concurrent` in-flight calls.
    /// Values below 1 are clamped to 1, mirroring the Go port.
    pub fn new(max_concurrent: usize) -> Self {
        let max_concurrent = max_concurrent.max(1);
        Self {
            sem: Semaphore::new(max_concurrent),
            max_concurrent,
        }
    }

    /// The (clamped) concurrency cap — pyfly's `max_concurrent` property.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Currently available slots — pyfly's `available_slots` property.
    pub fn available_slots(&self) -> usize {
        self.sem.available_permits()
    }

    /// Acquires a slot, runs `op`, releases. Blocks (asynchronously) until a
    /// slot frees up. Where the Go port aborts on `ctx` cancellation, the
    /// Rust analogue is dropping the returned future.
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ResilienceError>>,
    {
        let _permit = self
            .sem
            .acquire()
            .await
            .expect("bulkhead semaphore is never closed");
        op().await
    }

    /// The non-blocking variant — returns [`ResilienceError::BulkheadFull`]
    /// if no slot is available.
    pub async fn try_execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ResilienceError>>,
    {
        match self.sem.try_acquire() {
            Ok(_permit) => op().await,
            Err(_) => Err(ResilienceError::BulkheadFull),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    /// Port of Go `TestBulkheadCapsConcurrency`.
    #[tokio::test]
    async fn caps_concurrency() {
        let bh = Arc::new(Bulkhead::new(2));
        let inflight = Arc::new(AtomicI32::new(0));
        let max = Arc::new(AtomicI32::new(0));

        let mut handles = Vec::new();
        for _ in 0..5 {
            let bh = bh.clone();
            let inflight = inflight.clone();
            let max = max.clone();
            handles.push(tokio::spawn(async move {
                bh.execute(|| async {
                    let cur = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    max.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert!(
            max.load(Ordering::SeqCst) <= 2,
            "concurrency exceeded: {}",
            max.load(Ordering::SeqCst)
        );
    }

    /// Port of Go `TestBulkheadTryExecuteFailsWhenFull` — the Go test's
    /// 5 ms sleep is replaced by a deterministic notify handshake.
    #[tokio::test]
    async fn try_execute_fails_when_full() {
        let bh = Arc::new(Bulkhead::new(1));
        let acquired = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());

        let holder = {
            let bh = bh.clone();
            let acquired = acquired.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                bh.execute(|| async {
                    acquired.notify_one();
                    gate.notified().await;
                    Ok(())
                })
                .await
            })
        };

        acquired.notified().await;
        let err = bh.try_execute(|| async { Ok(()) }).await.unwrap_err();
        assert!(err.is_bulkhead_full(), "want BulkheadFull: {err}");

        gate.notify_one();
        holder.await.unwrap().unwrap();

        // Slot freed → try_execute succeeds again.
        bh.try_execute(|| async { Ok(()) }).await.unwrap();
    }

    #[tokio::test]
    async fn zero_capacity_clamps_to_one() {
        let bh = Bulkhead::new(0);
        bh.try_execute(|| async { Ok(()) }).await.unwrap();
    }

    #[tokio::test]
    async fn execute_propagates_operation_error() {
        let bh = Bulkhead::new(1);
        let err = bh
            .execute(|| async { Err::<(), _>(ResilienceError::operation("boom")) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }
}
