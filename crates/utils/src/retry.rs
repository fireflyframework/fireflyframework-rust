//! Exponential-backoff retry with jitter — the async Rust port of the
//! Go `utils.Do` / `utils.DoValue` helpers.
//!
//! Where Go threads a `context.Context` through the retried function
//! for cancellation, async Rust cancels by dropping the future: wrap a
//! [`retry`] call in `tokio::time::timeout` or a `select!` to bound it.

use std::future::Future;
use std::time::Duration;

/// Controls [`retry`]'s behaviour. The defaults — 3 attempts, 100 ms
/// initial delay, 2× multiplier, 5 s cap, ±20 % jitter — match the
/// Java RetryUtils defaults (and the Go/.NET/Python ports).
///
/// Go's `RetryConfig.RetryableErr` predicate is expressed in Rust as
/// the explicit [`retry_if`] variant instead of a config field, so the
/// config stays `Copy` and error-type agnostic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryConfig {
    /// Maximum number of attempts; `0` is normalised to `1`.
    pub max_attempts: u32,
    /// Delay before the second attempt; zero is normalised to 100 ms.
    pub initial_delay: Duration,
    /// Upper bound on any single backoff delay; zero is normalised to
    /// 30 s (the same fallback Go applies to a zero `MaxDelay`).
    pub max_delay: Duration,
    /// Exponential growth factor per attempt; values below `1.0` are
    /// normalised to `2.0`.
    pub multiplier: f64,
    /// Jitter as a fraction of the computed delay, clamped to `0..=1`;
    /// `0.2` means ±20 %.
    pub jitter_ratio: f64,
}

impl Default for RetryConfig {
    /// Returns the canonical retry policy, mirroring Go's
    /// `utils.DefaultRetry()`.
    fn default() -> Self {
        RetryConfig {
            max_attempts: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            multiplier: 2.0,
            jitter_ratio: 0.2,
        }
    }
}

impl RetryConfig {
    /// Applies the same defensive normalisation Go's `Do` performs on
    /// entry: at least one attempt, sane delays, multiplier ≥ 1.
    fn normalized(mut self) -> Self {
        if self.max_attempts == 0 {
            self.max_attempts = 1;
        }
        if self.initial_delay.is_zero() {
            self.initial_delay = Duration::from_millis(100);
        }
        if self.multiplier < 1.0 {
            self.multiplier = 2.0;
        }
        if self.max_delay.is_zero() {
            self.max_delay = Duration::from_secs(30);
        }
        self
    }
}

/// Retries `f` with exponential backoff until it succeeds or
/// `cfg.max_attempts` is reached. The final error is returned. The
/// async Rust port of Go's `utils.Do` and `utils.DoValue` — Rust's
/// `Result<T, E>` collapses both into one function (use `T = ()` for
/// the value-less form).
///
/// Every error is treated as retryable; use [`retry_if`] to
/// short-circuit on non-retryable errors.
pub async fn retry<T, E, F, Fut>(cfg: RetryConfig, f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    retry_if(cfg, |_| true, f).await
}

/// Like [`retry`], but consults `retryable` after each failure: if it
/// returns `false` the error is returned immediately without further
/// attempts — useful for non-retryable 4xx responses. The Rust
/// counterpart of Go's `RetryConfig.RetryableErr` predicate.
pub async fn retry_if<T, E, F, Fut, P>(cfg: RetryConfig, retryable: P, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    P: Fn(&E) -> bool,
{
    let cfg = cfg.normalized();
    let mut last_err: Option<E> = None;
    for attempt in 0..cfg.max_attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !retryable(&e) {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
        if attempt == cfg.max_attempts - 1 {
            break;
        }
        tokio::time::sleep(backoff_delay(&cfg, attempt)).await;
    }
    Err(last_err.expect("retry: at least one attempt is always made"))
}

/// Computes the backoff delay after the given zero-based attempt:
/// `initial_delay × multiplier^attempt`, capped at `max_delay`, then
/// jittered by a uniform factor in `[1 − jitter, 1 + jitter]`.
fn backoff_delay(cfg: &RetryConfig, attempt: u32) -> Duration {
    let mut base = cfg.initial_delay.as_secs_f64() * cfg.multiplier.powi(attempt as i32);
    let max = cfg.max_delay.as_secs_f64();
    if base > max {
        base = max;
    }
    if cfg.jitter_ratio > 0.0 {
        let j = cfg.jitter_ratio.min(1.0);
        // rand in [-j, +j]
        let factor = 1.0 + (rand::random::<f64>() * 2.0 - 1.0) * j;
        base *= factor;
        if base < 0.0 {
            base = 0.0;
        }
    }
    Duration::from_secs_f64(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_cfg() -> RetryConfig {
        RetryConfig {
            max_attempts: 4,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            jitter_ratio: 0.0,
            ..RetryConfig::default()
        }
    }

    /// Port of Go `TestRetry` (first half): transient failures are
    /// retried until success; exactly three attempts are made.
    #[tokio::test]
    async fn retry_recovers_from_transient_errors() {
        let mut attempts = 0u32;
        let result = retry(fast_cfg(), || {
            attempts += 1;
            let ok = attempts >= 3;
            async move {
                if ok {
                    Ok(())
                } else {
                    Err("transient")
                }
            }
        })
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(attempts, 3);
    }

    /// Port of Go `TestRetry` (second half): a non-retryable error
    /// short-circuits after a single call.
    #[tokio::test]
    async fn retry_if_short_circuits_non_retryable() {
        let mut calls = 0u32;
        let err = retry_if(
            fast_cfg(),
            |e: &&str| *e != "stop",
            || {
                calls += 1;
                async { Err::<(), _>("stop") }
            },
        )
        .await
        .expect_err("expected error");
        assert_eq!(calls, 1);
        assert_eq!(err, "stop");
    }

    /// Port of Go `TestRetryValue`: the typed form returns the value.
    #[tokio::test]
    async fn retry_returns_value() {
        let cfg = RetryConfig {
            max_attempts: 2,
            initial_delay: Duration::from_millis(1),
            jitter_ratio: 0.0,
            ..RetryConfig::default()
        };
        let v = retry(cfg, || async { Ok::<_, std::io::Error>("ok") }).await;
        assert_eq!(v.unwrap(), "ok");
    }

    /// Rust-specific: when every attempt fails, the last error is
    /// returned and exactly `max_attempts` calls are made.
    #[tokio::test]
    async fn retry_exhausts_attempts_and_returns_last_error() {
        let mut calls = 0u32;
        let err = retry(fast_cfg(), || {
            calls += 1;
            let n = calls;
            async move { Err::<(), String>(format!("fail #{n}")) }
        })
        .await
        .expect_err("expected error");
        assert_eq!(calls, 4);
        assert_eq!(err, "fail #4");
    }

    /// Rust-specific: `max_attempts == 0` is normalised to a single
    /// attempt, mirroring Go's `MaxAttempts <= 0` guard.
    #[tokio::test]
    async fn retry_normalizes_zero_attempts_to_one() {
        let cfg = RetryConfig {
            max_attempts: 0,
            initial_delay: Duration::from_millis(1),
            jitter_ratio: 0.0,
            ..RetryConfig::default()
        };
        let mut calls = 0u32;
        let _ = retry(cfg, || {
            calls += 1;
            async { Err::<(), _>("nope") }
        })
        .await;
        assert_eq!(calls, 1);
    }

    /// Config normalisation matches the Go `Do` entry guards.
    #[test]
    fn config_normalization_matches_go() {
        let n = RetryConfig {
            max_attempts: 0,
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            multiplier: 0.5,
            jitter_ratio: 0.2,
        }
        .normalized();
        assert_eq!(n.max_attempts, 1);
        assert_eq!(n.initial_delay, Duration::from_millis(100));
        assert_eq!(n.max_delay, Duration::from_secs(30));
        assert_eq!(n.multiplier, 2.0);
    }

    /// Backoff grows exponentially and is capped at `max_delay`.
    #[test]
    fn backoff_grows_and_caps() {
        let cfg = RetryConfig {
            max_attempts: 5,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(300),
            multiplier: 2.0,
            jitter_ratio: 0.0,
        };
        assert_eq!(backoff_delay(&cfg, 0), Duration::from_millis(100));
        assert_eq!(backoff_delay(&cfg, 1), Duration::from_millis(200));
        assert_eq!(backoff_delay(&cfg, 2), Duration::from_millis(300)); // capped
        assert_eq!(backoff_delay(&cfg, 3), Duration::from_millis(300)); // still capped
    }

    /// Jitter stays within ±jitter_ratio of the base delay.
    #[test]
    fn backoff_jitter_stays_in_bounds() {
        let cfg = RetryConfig {
            jitter_ratio: 0.2,
            ..RetryConfig::default()
        };
        for _ in 0..50 {
            let d = backoff_delay(&cfg, 0);
            assert!(
                d >= Duration::from_millis(80) && d <= Duration::from_millis(120),
                "jittered delay out of bounds: {d:?}"
            );
        }
    }

    /// Defaults mirror Go's `DefaultRetry()`.
    #[test]
    fn default_matches_go_default_retry() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.max_attempts, 3);
        assert_eq!(cfg.initial_delay, Duration::from_millis(100));
        assert_eq!(cfg.max_delay, Duration::from_secs(5));
        assert_eq!(cfg.multiplier, 2.0);
        assert_eq!(cfg.jitter_ratio, 0.2);
    }

    /// Rust-specific: the retry future is Send so it can be spawned.
    #[test]
    fn retry_future_is_send() {
        fn assert_send<T: Send>(_: T) {}
        assert_send(retry(RetryConfig::default(), || async {
            Ok::<_, std::io::Error>(1)
        }));
    }
}
