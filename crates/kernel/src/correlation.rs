//! Correlation-id propagation and the canonical header names.

use std::future::Future;

/// The canonical header name used by every Firefly runtime — Java,
/// .NET, Go, Python, Rust — to propagate the correlation id.
pub const HEADER_CORRELATION_ID: &str = "X-Correlation-Id";

/// The canonical header name for idempotent POSTs.
pub const HEADER_IDEMPOTENCY_KEY: &str = "Idempotency-Key";

tokio::task_local! {
    /// Task-local storage slot for the correlation id — the Rust analog
    /// of the Go port's `context.Context` value key.
    static CORRELATION_ID: String;
}

/// Runs `fut` with the given correlation id in scope — the Rust analog
/// of Go's `WithCorrelationID(ctx, id)`. Scopes nest: an inner scope
/// shadows the outer id, exactly like a child `context.Context`.
pub async fn with_correlation_id<F: Future>(id: impl Into<String>, fut: F) -> F::Output {
    CORRELATION_ID.scope(id.into(), fut).await
}

/// Runs the synchronous closure `f` with the given correlation id in
/// scope. Useful from blocking code and plain `#[test]` functions.
pub fn with_correlation_id_sync<F: FnOnce() -> R, R>(id: impl Into<String>, f: F) -> R {
    CORRELATION_ID.sync_scope(id.into(), f)
}

/// Extracts the correlation id from the current task-local scope,
/// returning `None` when no scope is active or the id is empty — the
/// Rust analog of Go's `CorrelationIDFrom(ctx)`.
pub fn correlation_id() -> Option<String> {
    CORRELATION_ID
        .try_with(Clone::clone)
        .ok()
        .filter(|id| !id.is_empty())
}

/// Returns a 32-character hex-encoded random id suitable for
/// `X-Correlation-Id` propagation.
pub fn new_correlation_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
