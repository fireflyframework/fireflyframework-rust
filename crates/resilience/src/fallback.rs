//! Fallback decorator for graceful degradation — port of pyfly's
//! `@fallback`.
//!
//! Where pyfly's decorator substitutes a `fallback_value` or calls a
//! `fallback_method` when the wrapped callable raises, the Rust analogue is a
//! [`Decorator`] for [`Chain`](crate::Chain): when the inner operation fails
//! with an error matched by the [`on`](Fallback::on) predicate, the handler
//! runs and its result replaces the operation's. `Chain` operations are
//! unit-valued, so "return a fallback value" becomes "recover with `Ok(())`"
//! (typically after a side effect such as serving a cached response) — for
//! value-returning calls plain `Result` combinators remain the idiomatic
//! tool, as the pyfly parity brief notes.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use crate::chain::{Decorator, Operation};
use crate::error::ResilienceError;

/// The boxed handler future.
type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), ResilienceError>> + Send>>;

/// The boxed fallback handler — receives the matched error by value.
type Handler = Arc<dyn Fn(ResilienceError) -> HandlerFuture + Send + Sync>;

/// The error-matching predicate — pyfly's `on=(ExceptionType, ...)` tuple.
type Predicate = Arc<dyn Fn(&ResilienceError) -> bool + Send + Sync>;

/// `Fallback` is the graceful-degradation decorator: it forwards successes
/// untouched and, when the inner operation (or an inner decorator's
/// short-circuit sentinel) fails with an error matched by the
/// [`on`](Self::on) predicate, invokes the handler — mirroring pyfly's
/// `fallback(fallback_method=..., on=(...))` semantics. Errors the predicate
/// rejects propagate unchanged.
///
/// ```
/// use firefly_resilience::{Chain, Fallback, ResilienceError};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// // Swallow timeouts only; every other error still propagates.
/// let chain = Chain::new().with(Fallback::recover().on(ResilienceError::is_timeout));
/// let out = chain
///     .execute(|| async { Err(ResilienceError::Timeout) })
///     .await;
/// assert!(out.is_ok());
/// # }
/// ```
#[derive(Clone)]
pub struct Fallback {
    handler: Handler,
    catches: Predicate,
}

impl fmt::Debug for Fallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fallback").finish_non_exhaustive()
    }
}

impl Fallback {
    /// Returns a fallback that swallows matched errors, recovering with
    /// `Ok(())` — the unit-`Chain` analogue of pyfly's static
    /// `fallback_value`.
    pub fn recover() -> Self {
        Self::new(|_| Ok(()))
    }

    /// Returns a fallback driven by a synchronous handler — the analogue of
    /// pyfly's `fallback_method`. The handler receives the matched error
    /// (pyfly's `exc` keyword argument) and may recover (`Ok(())`) or
    /// substitute another error.
    pub fn new<F>(handler: F) -> Self
    where
        F: Fn(ResilienceError) -> Result<(), ResilienceError> + Send + Sync + 'static,
    {
        Self {
            handler: Arc::new(move |err| {
                let out = handler(err);
                Box::pin(async move { out })
            }),
            catches: Arc::new(|_| true),
        }
    }

    /// Returns a fallback driven by an asynchronous handler — the analogue
    /// of pyfly awaiting an async `fallback_method`.
    pub fn new_async<F, Fut>(handler: F) -> Self
    where
        F: Fn(ResilienceError) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ResilienceError>> + Send + 'static,
    {
        Self {
            handler: Arc::new(move |err| Box::pin(handler(err))),
            catches: Arc::new(|_| true),
        }
    }

    /// Restricts the fallback to errors matched by `predicate` — pyfly's
    /// `on=(ExceptionType, ...)` filter. Unmatched errors propagate
    /// unchanged. The sentinel helpers compose directly:
    /// `.on(ResilienceError::is_timeout)`.
    pub fn on<P>(mut self, predicate: P) -> Self
    where
        P: Fn(&ResilienceError) -> bool + Send + Sync + 'static,
    {
        self.catches = Arc::new(predicate);
        self
    }
}

#[async_trait]
impl Decorator for Fallback {
    /// Forwards success; on a matched failure runs the handler, otherwise
    /// propagates the error untouched.
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError> {
        match op().await {
            Ok(()) => Ok(()),
            Err(err) if (self.catches)(&err) => (self.handler)(err).await,
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Chain;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Port of pyfly `test_success_bypasses_fallback`.
    #[tokio::test]
    async fn success_bypasses_fallback() {
        let invoked = Arc::new(AtomicBool::new(false));
        let flag = invoked.clone();
        let chain = Chain::new().with(Fallback::new(move |_| {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        }));
        chain.execute(|| async { Ok(()) }).await.unwrap();
        assert!(!invoked.load(Ordering::SeqCst), "fallback never invoked");
    }

    /// Port of pyfly `test_fallback_method_on_failure` — the handler is
    /// called exactly once with the failure and its result is returned.
    #[tokio::test]
    async fn fallback_method_on_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let chain = Chain::new().with(Fallback::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        chain
            .execute(|| async { Err(ResilienceError::operation("boom")) })
            .await
            .expect("handler recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Port of pyfly `test_fallback_value_on_failure` — `recover()` is the
    /// unit-Chain analogue of a static fallback_value.
    #[tokio::test]
    async fn recover_swallows_failure() {
        let chain = Chain::new().with(Fallback::recover());
        let out = chain
            .execute(|| async { Err(ResilienceError::operation("bad input")) })
            .await;
        assert!(out.is_ok());
    }

    /// Port of pyfly `test_specific_exception_types` — only matched errors
    /// are caught; others propagate.
    #[tokio::test]
    async fn specific_error_kinds_only() {
        let chain = Chain::new().with(Fallback::recover().on(ResilienceError::is_timeout));

        // Timeout is matched and recovered.
        chain
            .execute(|| async { Err(ResilienceError::Timeout) })
            .await
            .expect("timeout caught");

        // An operation error is not in `on`, so it propagates.
        let err = chain
            .execute(|| async { Err(ResilienceError::operation("error")) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "error");
    }

    /// Port of pyfly `test_fallback_method_receives_exception` — the handler
    /// sees the actual error instance.
    #[tokio::test]
    async fn fallback_method_receives_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let chain = Chain::new().with(Fallback::new(move |err| {
            sink.lock().unwrap().push(err.to_string());
            Ok(())
        }));
        chain
            .execute(|| async { Err(ResilienceError::operation("connection lost")) })
            .await
            .expect("recovered");
        assert_eq!(*captured.lock().unwrap(), vec!["connection lost"]);
    }

    /// Port of pyfly `test_async_fallback_method` — an async handler is
    /// awaited correctly.
    #[tokio::test]
    async fn async_fallback_method() {
        let invoked = Arc::new(AtomicBool::new(false));
        let flag = invoked.clone();
        let chain = Chain::new().with(Fallback::new_async(move |err| {
            let flag = flag.clone();
            async move {
                assert_eq!(err.to_string(), "missing");
                tokio::task::yield_now().await;
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }
        }));
        chain
            .execute(|| async { Err(ResilienceError::operation("missing")) })
            .await
            .expect("async handler recovered");
        assert!(invoked.load(Ordering::SeqCst));
    }

    /// The handler may substitute a different error instead of recovering.
    #[tokio::test]
    async fn handler_may_substitute_error() {
        let chain = Chain::new().with(Fallback::new(|err| {
            Err(ResilienceError::operation(format!("degraded: {err}")))
        }));
        let err = chain
            .execute(|| async { Err(ResilienceError::operation("boom")) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "degraded: boom");
    }

    /// Fallback catches inner decorators' short-circuit sentinels too —
    /// e.g. an open breaker downstream in the chain.
    #[tokio::test]
    async fn catches_inner_circuit_open_sentinel() {
        use crate::circuit_breaker::{CircuitBreaker, CircuitConfig, CircuitState};
        use std::time::Duration;

        let breaker = Arc::new(CircuitBreaker::new(CircuitConfig {
            failure_threshold: 1,
            window: Duration::ZERO,
            ..CircuitConfig::default()
        }));
        let recovered = Arc::new(AtomicUsize::new(0));
        let counter = recovered.clone();
        let chain = Chain::new()
            .with(Fallback::new(move |err| {
                assert!(err.is_circuit_open() || err.to_string() == "boom");
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))
            .with_shared(breaker.clone());

        // First call trips the breaker; fallback recovers the op failure.
        chain
            .execute(|| async { Err(ResilienceError::operation("boom")) })
            .await
            .unwrap();
        assert_eq!(breaker.state(), CircuitState::Open);

        // Second call is short-circuited by the breaker; fallback recovers.
        chain.execute(|| async { Ok(()) }).await.unwrap();
        assert_eq!(recovered.load(Ordering::SeqCst), 2);
    }
}
