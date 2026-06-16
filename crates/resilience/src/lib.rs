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

//! firefly-resilience — Resilience4j-equivalent decorators that compose
//! around any async operation.
//!
//! The crate ports the Go module `resilience` and provides five primitives
//! plus a combinator:
//!
//! | Primitive          | Failure mode it shields against             | Error variant                          |
//! |--------------------|---------------------------------------------|----------------------------------------|
//! | [`CircuitBreaker`] | Cascading failure of a slow / failing dep   | [`ResilienceError::CircuitOpen`]       |
//! | [`RateLimiter`]    | Outbound rate cap (token bucket)            | [`ResilienceError::RateLimited`]       |
//! | [`Bulkhead`]       | Resource exhaustion via runaway concurrency | [`ResilienceError::BulkheadFull`] (or block) |
//! | [`Timeout`]        | Stuck calls                                 | [`ResilienceError::Timeout`]           |
//! | [`Retry`]          | Transient failures (re-run with backoff)    | the operation's own error after exhaustion |
//!
//! [`Chain`] composes them into a single guarded call: decorators run
//! left-to-right, leftmost outermost — `Chain::new().with(timeout)
//! .with(breaker).with(bulkhead)` evaluates `timeout(breaker(bulkhead(call)))`.
//!
//! # pyfly parity layer
//!
//! On top of the Go-parity surface the crate ports pyfly's
//! `pyfly.resilience` extensions:
//!
//! * [`CircuitConfig`] gains a count-based **failure-rate window**
//!   (`failure_rate_threshold` + `window_size`, Resilience4j `COUNT_BASED`)
//!   and a **half-open probe budget** (`half_open_max_calls`); the historical
//!   consecutive-failures mode stays the default.
//! * [`CircuitBreaker`] exposes pyfly's manual hooks
//!   ([`before_call`](CircuitBreaker::before_call),
//!   [`on_success`](CircuitBreaker::on_success),
//!   [`on_failure`](CircuitBreaker::on_failure)).
//! * [`Fallback`] is the graceful-degradation decorator for [`Chain`].
//! * [`Retry`] is the declarative retry combinator (port of pyfly's `@retry`):
//!   re-runs a re-runnable async closure with exponential backoff, optional
//!   jitter, a per-attempt delay cap, and a [`retry_on`](Retry::retry_on)
//!   predicate (pyfly's `exceptions=`). The free function [`retry`] mirrors
//!   pyfly's `retry(...)` call shape.
//! * [`ResilienceRegistry`] materialises named breakers / limiters /
//!   bulkheads / time-limiters / **retries** from `firefly.resilience.*`
//!   configuration keys ([`ResilienceRegistry::from_config`]), with
//!   [`parse_duration`] accepting pyfly-style values (`"500ms"`, `"2.5"`,
//!   `"1m"`) plus anything `humantime` understands.
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
mod fallback;
mod rate_limiter;
mod registry;
mod retry;
mod timeout;

pub use bulkhead::Bulkhead;
pub use chain::{from_fn, operation, Chain, Decorator, FnDecorator, OpFuture, Operation};
pub use circuit_breaker::{CircuitBreaker, CircuitConfig, CircuitState, Clock};
pub use error::{BoxError, ResilienceError};
pub use fallback::Fallback;
pub use rate_limiter::RateLimiter;
pub use registry::{parse_duration, RegistryError, ResilienceRegistry};
pub use retry::{retry, JitterFn, Retry, RetryConfig, DEFAULT_MAX_ATTEMPTS};
pub use timeout::Timeout;

/// Framework version stamp.
pub const VERSION: &str = "26.6.19";

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
        assert_send_sync::<Fallback>();
        assert_send_sync::<Retry>();
        assert_send_sync::<RetryConfig>();
        assert_send_sync::<ResilienceRegistry>();
        assert_send_sync::<RegistryError>();
    }
}
