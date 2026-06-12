//! firefly-resilience — Resilience4j-equivalent decorators that compose
//! around any async operation.
//!
//! The crate ports the Go module `resilience` and provides four primitives
//! plus a combinator:
//!
//! | Primitive          | Failure mode it shields against             | Error variant                          |
//! |--------------------|---------------------------------------------|----------------------------------------|
//! | [`CircuitBreaker`] | Cascading failure of a slow / failing dep   | [`ResilienceError::CircuitOpen`]       |
//! | [`RateLimiter`]    | Outbound rate cap (token bucket)            | [`ResilienceError::RateLimited`]       |
//! | [`Bulkhead`]       | Resource exhaustion via runaway concurrency | [`ResilienceError::BulkheadFull`] (or block) |
//! | [`Timeout`]        | Stuck calls                                 | [`ResilienceError::Timeout`]           |
//!
//! [`Chain`] composes them into a single guarded call: decorators run
//! left-to-right, leftmost outermost — `Chain::new().with(timeout)
//! .with(breaker).with(bulkhead)` evaluates `timeout(breaker(bulkhead(call)))`.
//!
//! Error messages are byte-identical to the Go port's sentinels
//! (`firefly/resilience: circuit open`, …), so logs and dashboards stay
//! consistent across the sibling framework ports. Where Go threads a
//! `context.Context` through every call for cancellation, the Rust analogue
//! is dropping the future.
//!
//! # Quick start
//!
//! ```
//! use std::sync::Arc;
//! use std::time::Duration;
//! use firefly_resilience::{
//!     Bulkhead, Chain, CircuitBreaker, CircuitConfig, ResilienceError, Timeout,
//! };
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), ResilienceError> {
//! let breaker = Arc::new(CircuitBreaker::new(CircuitConfig::default()));
//!
//! let guarded = Chain::new()
//!     .with(Timeout::new(Duration::from_secs(2)))   // per-call deadline
//!     .with_shared(breaker.clone())                 // short-circuit when sick
//!     .with(Bulkhead::new(20));                     // cap in-flight calls
//!
//! guarded.execute(|| async { Ok(()) }).await?;
//! assert_eq!(breaker.state().to_string(), "closed");
//! # Ok(())
//! # }
//! ```

mod bulkhead;
mod chain;
mod circuit_breaker;
mod error;
mod rate_limiter;
mod timeout;

pub use bulkhead::Bulkhead;
pub use chain::{from_fn, operation, Chain, Decorator, FnDecorator, OpFuture, Operation};
pub use circuit_breaker::{CircuitBreaker, CircuitConfig, CircuitState, Clock};
pub use error::{BoxError, ResilienceError};
pub use rate_limiter::RateLimiter;
pub use timeout::Timeout;

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CircuitBreaker>();
        assert_send_sync::<RateLimiter>();
        assert_send_sync::<Bulkhead>();
        assert_send_sync::<Timeout>();
        assert_send_sync::<Chain>();
        assert_send_sync::<ResilienceError>();
    }
}
