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

//! Cooperative cancellation for orchestration runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cooperative cancellation flag shared between an orchestration run and
/// its caller — the Rust analogue of the Go port's `context.Context`
/// cancellation.
///
/// Cloning the token yields a handle to the same underlying flag.
/// Engines check the token between steps (saga) and waves (workflow);
/// cancellation is cooperative, never pre-emptive.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Creates a new, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Flags the token as cancelled. Idempotent; never blocks.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Reports whether [`cancel`](Self::cancel) has been called on this
    /// token or any of its clones.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_starts_uncancelled_and_clones_share_state() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        let clone = token.clone();
        token.cancel();
        assert!(token.is_cancelled());
        assert!(clone.is_cancelled());
        // Idempotent.
        token.cancel();
        assert!(token.is_cancelled());
    }
}
