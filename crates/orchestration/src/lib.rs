//! Distributed-transaction engines: Saga, Workflow (DAG), and TCC.
//!
//! `firefly-orchestration` ships the three classic distributed-transaction
//! engines every Firefly platform agrees on:
//!
//! | Engine       | Topology                   | Compensation                       |
//! |--------------|----------------------------|------------------------------------|
//! | [`Saga`]     | Sequential steps           | Reverse-order, configurable policy |
//! | [`Workflow`] | DAG with parallel branches | None — fail-fast                   |
//! | [`Tcc`]      | Try-all then Confirm-all   | Cancel-tried-on-Try-failure        |
//!
//! Each engine accepts a typed step / node / participant built from async
//! closures, runs as a plain future on the caller's task, and respects
//! cooperative cancellation through a [`CancellationToken`] — the Rust
//! analogue of the Go port's `context.Context` cancellation.
//!
//! # Quick start
//!
//! ```rust
//! use firefly_orchestration::{CompensationPolicy, Saga, SagaStatus, Step};
//!
//! let saga = Saga::new("checkout")
//!     .policy(CompensationPolicy::BestEffort)
//!     .step(
//!         Step::new("reserve", || async { Ok(()) })
//!             .with_compensation(|| async { Ok(()) }),
//!     )
//!     .step(
//!         Step::new("charge", || async { Ok(()) })
//!             .with_compensation(|| async { Ok(()) }),
//!     )
//!     .step(Step::new("ship", || async { Ok(()) }));
//!
//! let outcome = tokio::runtime::Runtime::new()
//!     .unwrap()
//!     .block_on(saga.run())
//!     .expect("saga completes");
//! assert_eq!(outcome.status, SagaStatus::Completed);
//! assert_eq!(outcome.steps_executed, ["reserve", "charge", "ship"]);
//! ```

mod cancel;
mod saga;
mod tcc;
mod workflow;

pub use cancel::CancellationToken;
pub use saga::{CompensationPolicy, Outcome, Saga, SagaError, SagaFailure, SagaStatus, Step};
pub use tcc::{ConfirmError, Tcc, TccError, TccParticipant};
pub use workflow::{Node, Workflow, WorkflowError};

use std::future::Future;

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

/// Boxed error returned by step / node / participant callbacks — the Rust
/// analogue of Go's `error` interface value.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Boxed future produced by an orchestration action callback.
pub type ActionFuture = futures::future::BoxFuture<'static, Result<(), BoxError>>;

/// Type-erased action callback stored by the engines.
pub(crate) type ActionFn = Box<dyn Fn() -> ActionFuture + Send + Sync>;

/// Boxes an async closure into the engines' type-erased callback form.
pub(crate) fn boxed_action<F, Fut>(f: F) -> ActionFn
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
{
    Box::new(move || -> ActionFuture { Box::pin(f()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_stamp() {
        assert_eq!(VERSION, "26.6.1");
    }

    #[test]
    fn engines_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Saga>();
        assert_send_sync::<Workflow>();
        assert_send_sync::<Tcc>();
        assert_send_sync::<Step>();
        assert_send_sync::<Node>();
        assert_send_sync::<TccParticipant>();
        assert_send_sync::<CancellationToken>();
        assert_send_sync::<Outcome>();
    }

    #[test]
    fn run_futures_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        let saga = Saga::new("s").step(Step::new("a", || async { Ok(()) }));
        assert_send(&saga.run());
        let workflow = Workflow::new("w").node(Node::new("a", || async { Ok(()) }));
        assert_send(&workflow.run());
        let tcc = Tcc::new("t");
        assert_send(&tcc.run());
    }
}
