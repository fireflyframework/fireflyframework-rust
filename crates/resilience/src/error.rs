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

//! Error taxonomy shared by every resilience decorator.

use std::error::Error as StdError;

/// Boxed dynamic error used to carry operation-level failures through the
/// decorators — the Rust analogue of the untyped `error` the Go port's
/// guarded functions return.
pub type BoxError = Box<dyn StdError + Send + Sync + 'static>;

/// `ResilienceError` is the crate's error enum. The four short-circuit
/// variants correspond 1:1 to the Go port's sentinel errors and render the
/// exact same messages, so logs stay greppable across the sibling ports:
///
/// | Variant        | Go sentinel       | Message                              |
/// |----------------|-------------------|--------------------------------------|
/// | [`CircuitOpen`](Self::CircuitOpen) | `ErrCircuitOpen`  | `firefly/resilience: circuit open`   |
/// | [`RateLimited`](Self::RateLimited) | `ErrRateLimited`  | `firefly/resilience: rate limited`   |
/// | [`BulkheadFull`](Self::BulkheadFull)| `ErrBulkheadFull` | `firefly/resilience: bulkhead full`  |
/// | [`Timeout`](Self::Timeout)     | `ErrTimeout`      | `firefly/resilience: timeout`        |
///
/// [`Operation`](Self::Operation) carries the guarded call's own failure —
/// where Go propagates `fn`'s error untouched, Rust wraps it so a single
/// error type can flow through [`Chain`](crate::Chain) composition. Its
/// `Display` defers to the inner error, keeping messages identical.
#[derive(Debug, thiserror::Error)]
pub enum ResilienceError {
    /// The circuit breaker is in the `Open` state and short-circuited the
    /// call before it ran.
    #[error("firefly/resilience: circuit open")]
    CircuitOpen,

    /// The rate limiter's token bucket was empty.
    #[error("firefly/resilience: rate limited")]
    RateLimited,

    /// A non-blocking bulkhead acquisition found no free slot.
    #[error("firefly/resilience: bulkhead full")]
    BulkheadFull,

    /// The guarded call did not complete within the timeout budget.
    #[error("firefly/resilience: timeout")]
    Timeout,

    /// A failure produced by the guarded operation itself; displays the
    /// inner error verbatim.
    #[error("{0}")]
    Operation(BoxError),
}

impl ResilienceError {
    /// Wraps an operation-level failure. Accepts anything convertible into a
    /// [`BoxError`] — concrete error types, `String`, or `&str`.
    pub fn operation(err: impl Into<BoxError>) -> Self {
        Self::Operation(err.into())
    }

    /// Returns `true` if this is the circuit-open sentinel — the Rust
    /// analogue of `errors.Is(err, ErrCircuitOpen)`.
    pub fn is_circuit_open(&self) -> bool {
        matches!(self, Self::CircuitOpen)
    }

    /// Returns `true` if this is the rate-limited sentinel.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::RateLimited)
    }

    /// Returns `true` if this is the bulkhead-full sentinel.
    pub fn is_bulkhead_full(&self) -> bool {
        matches!(self, Self::BulkheadFull)
    }

    /// Returns `true` if this is the timeout sentinel.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_messages_match_go_port() {
        assert_eq!(
            ResilienceError::CircuitOpen.to_string(),
            "firefly/resilience: circuit open"
        );
        assert_eq!(
            ResilienceError::RateLimited.to_string(),
            "firefly/resilience: rate limited"
        );
        assert_eq!(
            ResilienceError::BulkheadFull.to_string(),
            "firefly/resilience: bulkhead full"
        );
        assert_eq!(
            ResilienceError::Timeout.to_string(),
            "firefly/resilience: timeout"
        );
    }

    #[test]
    fn operation_displays_inner_error_verbatim() {
        let err = ResilienceError::operation("boom");
        assert_eq!(err.to_string(), "boom");

        let io = std::io::Error::other("disk on fire");
        let err = ResilienceError::operation(io);
        assert_eq!(err.to_string(), "disk on fire");
    }

    #[test]
    fn is_helpers_discriminate_variants() {
        assert!(ResilienceError::CircuitOpen.is_circuit_open());
        assert!(ResilienceError::RateLimited.is_rate_limited());
        assert!(ResilienceError::BulkheadFull.is_bulkhead_full());
        assert!(ResilienceError::Timeout.is_timeout());

        let op = ResilienceError::operation("boom");
        assert!(!op.is_circuit_open());
        assert!(!op.is_rate_limited());
        assert!(!op.is_bulkhead_full());
        assert!(!op.is_timeout());
    }
}
