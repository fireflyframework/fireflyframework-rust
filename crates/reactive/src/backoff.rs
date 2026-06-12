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

//! Retry-backoff policy — the Rust analog of Reactor's
//! `reactor.util.retry.Retry.backoff(..)`.
//!
//! Used by [`Mono::retry_backoff`](crate::Mono::retry_backoff) and
//! [`Flux::retry_backoff`](crate::Flux::retry_backoff): on each failed
//! attempt the source is re-subscribed after a delay that grows
//! exponentially from a base, doubling per attempt, capped at an
//! optional ceiling.

use std::time::Duration;

/// An exponential-backoff retry policy.
///
/// ```
/// use std::time::Duration;
/// use firefly_reactive::Backoff;
///
/// let b = Backoff::new(3, Duration::from_millis(100));
/// assert_eq!(b.delay_for(0), Duration::from_millis(100));
/// assert_eq!(b.delay_for(1), Duration::from_millis(200));
/// assert_eq!(b.delay_for(2), Duration::from_millis(400));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Maximum number of *retries* (re-subscriptions) after the first
    /// attempt. `0` means no retry.
    pub max_retries: u32,
    /// The base delay before the first retry; doubles each attempt.
    pub base: Duration,
    /// Optional ceiling on the per-attempt delay.
    pub max_delay: Option<Duration>,
}

impl Backoff {
    /// Builds a policy with `max_retries` retries and a `base` first
    /// delay (doubling each attempt, no ceiling).
    pub fn new(max_retries: u32, base: Duration) -> Self {
        Self {
            max_retries,
            base,
            max_delay: None,
        }
    }

    /// Returns the policy with an upper bound on the per-attempt delay.
    #[must_use]
    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = Some(max_delay);
        self
    }

    /// The delay before the retry numbered `attempt` (0-based: the first
    /// retry is `attempt == 0`). Computed as `base * 2^attempt`, clamped
    /// to [`max_delay`](Backoff::max_delay) and saturating on overflow.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
        let millis = (self.base.as_millis() as u64).saturating_mul(factor);
        let delay = Duration::from_millis(millis);
        match self.max_delay {
            Some(cap) if delay > cap => cap,
            _ => delay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_each_attempt() {
        let b = Backoff::new(5, Duration::from_millis(10));
        assert_eq!(b.delay_for(0), Duration::from_millis(10));
        assert_eq!(b.delay_for(1), Duration::from_millis(20));
        assert_eq!(b.delay_for(2), Duration::from_millis(40));
        assert_eq!(b.delay_for(3), Duration::from_millis(80));
    }

    #[test]
    fn respects_max_delay() {
        let b =
            Backoff::new(5, Duration::from_millis(10)).with_max_delay(Duration::from_millis(25));
        assert_eq!(b.delay_for(0), Duration::from_millis(10));
        assert_eq!(b.delay_for(1), Duration::from_millis(20));
        assert_eq!(b.delay_for(2), Duration::from_millis(25));
        assert_eq!(b.delay_for(10), Duration::from_millis(25));
    }

    #[test]
    fn saturates_on_overflow() {
        let b = Backoff::new(100, Duration::from_secs(1));
        // very large attempt should not panic
        let _ = b.delay_for(80);
    }
}
