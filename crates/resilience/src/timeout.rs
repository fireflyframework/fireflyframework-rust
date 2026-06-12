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

//! Per-call deadline wrapper.

use std::future::Future;
use std::time::Duration;

use crate::error::ResilienceError;

/// `Timeout` enforces a per-call deadline. Where the Go port runs `fn` in its
/// own goroutine (leaving it running after the deadline), the Rust port
/// cancels the operation's future outright on budget exhaustion — the
/// caller-visible contract is identical: [`ResilienceError::Timeout`] when
/// the budget is exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout {
    /// The per-call deadline. [`Duration::ZERO`] disables the deadline and
    /// runs operations directly, mirroring the Go port's `Budget <= 0` path.
    pub budget: Duration,
}

impl Timeout {
    /// Returns a `Timeout` with the given budget.
    pub fn new(budget: Duration) -> Self {
        Self { budget }
    }

    /// Runs `op` and returns [`ResilienceError::Timeout`] if it does not
    /// complete within the budget. A zero budget runs `op` without a
    /// deadline.
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ResilienceError>>,
    {
        if self.budget.is_zero() {
            return op().await;
        }
        match tokio::time::timeout(self.budget, op()).await {
            Ok(result) => result,
            Err(_elapsed) => Err(ResilienceError::Timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go `TestTimeoutFires`.
    #[tokio::test]
    async fn fires_on_budget_exceeded() {
        let to = Timeout::new(Duration::from_millis(20));
        let err = to
            .execute(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(err.is_timeout(), "want Timeout: {err}");
    }

    #[tokio::test]
    async fn zero_budget_runs_directly() {
        let to = Timeout::new(Duration::ZERO);
        let value = to.execute(|| async { Ok(7) }).await.unwrap();
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn passes_through_within_budget() {
        let to = Timeout::new(Duration::from_millis(50));
        let value = to.execute(|| async { Ok("done") }).await.unwrap();
        assert_eq!(value, "done");

        let err = to
            .execute(|| async { Err::<(), _>(ResilienceError::operation("boom")) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }
}
