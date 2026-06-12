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
