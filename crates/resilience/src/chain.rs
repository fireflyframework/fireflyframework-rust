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

//! Decorator trait and the `Chain` combinator.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use crate::bulkhead::Bulkhead;
use crate::circuit_breaker::CircuitBreaker;
use crate::error::ResilienceError;
use crate::rate_limiter::RateLimiter;
use crate::timeout::Timeout;

/// The boxed future an [`Operation`] yields once invoked.
pub type OpFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ResilienceError>> + Send + 'a>>;

/// `Operation` is the boxed guarded call — the Rust analogue of the Go
/// port's `func() error`. [`operation`] boxes an async closure into one.
pub type Operation<'a> = Box<dyn FnOnce() -> OpFuture<'a> + Send + 'a>;

/// Boxes an async closure into an [`Operation`] suitable for
/// [`Decorator::call`].
pub fn operation<'a, F, Fut>(f: F) -> Operation<'a>
where
    F: FnOnce() -> Fut + Send + 'a,
    Fut: Future<Output = Result<(), ResilienceError>> + Send + 'a,
{
    Box::new(move || -> OpFuture<'a> { Box::pin(f()) })
}

/// `Decorator` is the Resilience4j-style call wrapper: each decorator wraps
/// the inner operation, optionally short-circuiting before it runs.
///
/// This trait replaces both the Go port's `Decorator` func type and its
/// `AsDecorator` adapter — [`CircuitBreaker`], [`RateLimiter`], [`Bulkhead`],
/// [`Timeout`], and [`Chain`] all implement it directly, and free functions
/// adapt via [`from_fn`].
#[async_trait]
pub trait Decorator: Send + Sync {
    /// Wraps `op`, optionally short-circuiting before it runs.
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError>;
}

#[async_trait]
impl Decorator for CircuitBreaker {
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
        self.execute(op).await
    }
}

#[async_trait]
impl Decorator for RateLimiter {
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
        self.execute(op).await
    }
}

#[async_trait]
impl Decorator for Bulkhead {
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
        self.execute(op).await
    }
}

#[async_trait]
impl Decorator for Timeout {
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
        self.execute(op).await
    }
}

/// `FnDecorator` adapts a plain (higher-ranked) function into a
/// [`Decorator`] — the closest Rust analogue of using a bare Go func as a
/// `Decorator`. Construct it with [`from_fn`].
pub struct FnDecorator<F>(F);

/// Adapts a function of shape `for<'a> Fn(Operation<'a>) -> OpFuture<'a>`
/// into a [`Decorator`]. Plain `fn` items coerce directly:
///
/// ```
/// use firefly_resilience::{from_fn, Chain, OpFuture, Operation};
///
/// fn passthrough(op: Operation<'_>) -> OpFuture<'_> {
///     Box::pin(async move { op().await })
/// }
///
/// let chain = Chain::new().with(from_fn(passthrough));
/// ```
pub fn from_fn<F>(f: F) -> FnDecorator<F>
where
    F: for<'a> Fn(Operation<'a>) -> OpFuture<'a> + Send + Sync,
{
    FnDecorator(f)
}

#[async_trait]
impl<F> Decorator for FnDecorator<F>
where
    F: for<'a> Fn(Operation<'a>) -> OpFuture<'a> + Send + Sync,
{
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
        (self.0)(op).await
    }
}

/// `Chain` composes decorators left-to-right — the first decorator added
/// runs outermost, exactly like the Go port's `Chain(decorators...)`:
///
/// ```text
/// Chain::new().with(timeout).with(breaker).with(bulkhead).execute(call)
///   = timeout(breaker(bulkhead(call)))
/// ```
#[derive(Default, Clone)]
pub struct Chain {
    decorators: Vec<Arc<dyn Decorator>>,
}

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chain")
            .field("decorators", &self.decorators.len())
            .finish()
    }
}

impl Chain {
    /// Returns an empty chain — executing it runs the operation directly.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a decorator the chain takes ownership of. The first decorator
    /// added runs outermost.
    pub fn with(mut self, decorator: impl Decorator + 'static) -> Self {
        self.decorators.push(Arc::new(decorator));
        self
    }

    /// Appends a shared decorator — use this to keep a handle (e.g. an
    /// `Arc<CircuitBreaker>` for state inspection) while the chain guards
    /// calls through it.
    pub fn with_shared(mut self, decorator: Arc<dyn Decorator>) -> Self {
        self.decorators.push(decorator);
        self
    }

    /// Runs `op` through the composed decorators, leftmost outermost.
    pub async fn execute<F, Fut>(&self, op: F) -> Result<(), ResilienceError>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<(), ResilienceError>> + Send,
    {
        nest(&self.decorators, operation(op)).await
    }
}

#[async_trait]
impl Decorator for Chain {
    /// A chain is itself a decorator, so chains nest inside other chains.
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
        // Re-box to unify the `&self` and `op` lifetimes — `Operation` is
        // invariant over its lifetime parameter, so it cannot shrink by
        // coercion alone.
        nest(&self.decorators, operation(op)).await
    }
}

/// Recursively wraps `op` so that `decorators[0]` runs outermost — the Rust
/// analogue of the Go port's right-to-left closure fold in `Chain`.
fn nest<'a>(decorators: &'a [Arc<dyn Decorator>], op: Operation<'a>) -> OpFuture<'a> {
    match decorators.split_first() {
        None => op(),
        Some((outer, rest)) => {
            Box::pin(async move { outer.call(Box::new(move || nest(rest, op))).await })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit_breaker::{CircuitConfig, CircuitState};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    /// Test decorator that records pre/post markers around the inner call.
    struct Recording {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Decorator for Recording {
        async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
            self.log.lock().unwrap().push(format!("{}-pre", self.name));
            let result = op().await;
            self.log.lock().unwrap().push(format!("{}-post", self.name));
            result
        }
    }

    /// Port of Go `TestChainOrder` — leftmost decorator runs outermost.
    #[tokio::test]
    async fn chain_runs_decorators_left_to_right() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let chain = Chain::new()
            .with(Recording {
                name: "d1",
                log: log.clone(),
            })
            .with(Recording {
                name: "d2",
                log: log.clone(),
            });

        let inner = log.clone();
        chain
            .execute(move || async move {
                inner.lock().unwrap().push("fn".to_string());
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            vec!["d1-pre", "d2-pre", "fn", "d2-post", "d1-post"]
        );
    }

    #[tokio::test]
    async fn empty_chain_runs_operation_directly() {
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        Chain::new()
            .execute(move || async move {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();
        assert!(ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn chain_short_circuits_on_open_breaker() {
        let breaker = Arc::new(CircuitBreaker::new(CircuitConfig {
            failure_threshold: 1,
            window: Duration::ZERO,
            open_duration: Duration::from_secs(30),
            ..CircuitConfig::default()
        }));
        let chain = Chain::new().with_shared(breaker.clone());

        let err = chain
            .execute(|| async { Err(ResilienceError::operation("boom")) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
        assert_eq!(breaker.state(), CircuitState::Open);

        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let err = chain
            .execute(move || async move {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(err.is_circuit_open(), "want CircuitOpen: {err}");
        assert!(!ran.load(Ordering::SeqCst), "op must not run when open");
    }

    /// The README's canonical guarded call: timeout → breaker → bulkhead.
    #[tokio::test]
    async fn chain_composes_all_primitives() {
        let breaker = Arc::new(CircuitBreaker::new(CircuitConfig::default()));
        let chain = Chain::new()
            .with(Timeout::new(Duration::from_millis(100)))
            .with_shared(breaker.clone())
            .with(Bulkhead::new(2))
            .with(RateLimiter::new(100.0, 10));

        chain.execute(|| async { Ok(()) }).await.unwrap();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    /// A sentinel from an inner decorator is a failure for the outer breaker,
    /// matching the Go port where any non-nil error is recorded.
    #[tokio::test]
    async fn inner_sentinel_counts_as_breaker_failure() {
        let breaker = Arc::new(CircuitBreaker::new(CircuitConfig {
            failure_threshold: 1,
            window: Duration::ZERO,
            open_duration: Duration::from_secs(30),
            ..CircuitConfig::default()
        }));
        let rl = RateLimiter::new(0.001, 0); // always empty
        let chain = Chain::new().with_shared(breaker.clone()).with(rl);

        let err = chain.execute(|| async { Ok(()) }).await.unwrap_err();
        assert!(err.is_rate_limited());
        assert_eq!(breaker.state(), CircuitState::Open);
    }

    #[tokio::test]
    async fn chains_nest_as_decorators() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let inner_chain = Chain::new().with(Recording {
            name: "inner",
            log: log.clone(),
        });
        let chain = Chain::new()
            .with(Recording {
                name: "outer",
                log: log.clone(),
            })
            .with(inner_chain);

        let marker = log.clone();
        chain
            .execute(move || async move {
                marker.lock().unwrap().push("fn".to_string());
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            vec!["outer-pre", "inner-pre", "fn", "inner-post", "outer-post"]
        );
    }

    fn passthrough(op: Operation<'_>) -> OpFuture<'_> {
        Box::pin(async move { op().await })
    }

    #[tokio::test]
    async fn from_fn_adapts_function_decorators() {
        let chain = Chain::new().with(from_fn(passthrough));
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        chain
            .execute(move || async move {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();
        assert!(ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn timeout_decorator_fires_inside_chain() {
        let chain = Chain::new().with(Timeout::new(Duration::from_millis(20)));
        let err = chain
            .execute(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(err.is_timeout(), "want Timeout: {err}");
    }
}
