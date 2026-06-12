//! Token-bucket rate limiter.

use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::ResilienceError;

/// Mutable bucket state guarded by the mutex.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_tick: Instant,
}

/// `RateLimiter` is a simple token-bucket limiter — `burst` tokens replenish
/// at `rate` per second. It shields downstream dependencies from an outbound
/// rate overshoot.
#[derive(Debug)]
pub struct RateLimiter {
    rate: f64,
    burst: f64,
    bucket: Mutex<Bucket>,
}

impl RateLimiter {
    /// Returns a limiter that emits `rate` tokens per second up to `burst`
    /// accumulated tokens. The bucket starts full. `rate` must be positive
    /// for [`wait`](Self::wait) to make progress.
    pub fn new(rate: f64, burst: usize) -> Self {
        Self {
            rate,
            burst: burst as f64,
            bucket: Mutex::new(Bucket {
                tokens: burst as f64,
                last_tick: Instant::now(),
            }),
        }
    }

    /// Returns `true` if a token is available, consuming it. Non-blocking.
    pub fn allow(&self) -> bool {
        let mut bucket = self.bucket.lock().expect("rate limiter mutex poisoned");
        self.refill(&mut bucket);
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }

    /// Runs `op` iff a token is available; otherwise returns
    /// [`ResilienceError::RateLimited`].
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ResilienceError>>,
    {
        if !self.allow() {
            return Err(ResilienceError::RateLimited);
        }
        op().await
    }

    /// Blocks (asynchronously) until a token is available, consuming it.
    /// Where the Go port aborts on `ctx` cancellation, the Rust analogue is
    /// dropping the returned future.
    pub async fn wait(&self) {
        loop {
            let wait = {
                let mut bucket = self.bucket.lock().expect("rate limiter mutex poisoned");
                self.refill(&mut bucket);
                if bucket.tokens >= 1.0 {
                    bucket.tokens -= 1.0;
                    return;
                }
                let need = 1.0 - bucket.tokens;
                // Defensive: a non-positive / non-finite rate yields a short
                // retry sleep instead of a panicking Duration conversion.
                Duration::try_from_secs_f64(need / self.rate).unwrap_or(Duration::from_millis(1))
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Credits the bucket with tokens accrued since the last tick, capping
    /// at `burst`.
    fn refill(&self, bucket: &mut Bucket) {
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_tick).as_secs_f64();
        bucket.last_tick = now;
        bucket.tokens += elapsed * self.rate;
        if bucket.tokens > self.burst {
            bucket.tokens = self.burst;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go `TestRateLimiterAllow`.
    #[test]
    fn allow_consumes_burst_then_denies() {
        let rl = RateLimiter::new(100.0, 3); // 100 tps, burst 3
        let mut count = 0;
        for _ in 0..5 {
            if rl.allow() {
                count += 1;
            }
        }
        assert_eq!(count, 3);
    }

    /// Port of Go `TestRateLimiterWait`.
    #[tokio::test]
    async fn wait_blocks_for_refill() {
        let rl = RateLimiter::new(50.0, 1); // 50 tps, burst 1
        rl.wait().await; // burst token: immediate
        let start = Instant::now();
        rl.wait().await; // must wait ~20 ms for the next token
        assert!(
            start.elapsed() >= Duration::from_millis(10),
            "wait should have blocked for refill, took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn execute_returns_rate_limited_when_empty() {
        let rl = RateLimiter::new(0.001, 1); // effectively no refill
        rl.execute(|| async { Ok(()) }).await.unwrap();
        let err = rl.execute(|| async { Ok(()) }).await.unwrap_err();
        assert!(err.is_rate_limited(), "want RateLimited: {err}");
    }

    #[tokio::test]
    async fn tokens_cap_at_burst() {
        let rl = RateLimiter::new(1000.0, 2);
        assert!(rl.allow());
        assert!(rl.allow());
        // 5 ms at 1000 tps would refill 5 tokens uncapped — the burst cap
        // must hold it to 2.
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(rl.allow());
        assert!(rl.allow());
        assert!(!rl.allow(), "tokens must cap at burst");
    }
}
