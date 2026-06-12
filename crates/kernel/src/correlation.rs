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

//! Correlation-id / request-id / tenant-id propagation and the
//! canonical header names.
//!
//! Three identifiers cover the cross-service correlation surface,
//! mirroring pyfly's `pyfly.observability.correlation` context vars:
//!
//! * `X-Correlation-Id` — end-to-end correlation across service hops.
//!   Echoed verbatim when supplied; generated when absent.
//! * `X-Request-Id` — one identifier per HTTP call. Generated when
//!   absent.
//! * `X-Tenant-Id` — multi-tenant scope. Never generated server-side;
//!   absent means "unscoped".

use std::future::Future;

/// The canonical header name used by every Firefly runtime — Java,
/// .NET, Go, Python, Rust — to propagate the correlation id.
pub const HEADER_CORRELATION_ID: &str = "X-Correlation-Id";

/// The canonical header name for idempotent POSTs.
pub const HEADER_IDEMPOTENCY_KEY: &str = "Idempotency-Key";

/// The canonical header name for the per-HTTP-call request id —
/// pyfly's `REQUEST_ID_HEADER`.
pub const HEADER_REQUEST_ID: &str = "X-Request-Id";

/// The canonical header name for the multi-tenant scope — pyfly's
/// `TENANT_ID_HEADER`. Never generated server-side.
pub const HEADER_TENANT_ID: &str = "X-Tenant-Id";

tokio::task_local! {
    /// Task-local storage slot for the correlation id — the Rust analog
    /// of the Go port's `context.Context` value key.
    static CORRELATION_ID: String;

    /// Task-local storage slot for the per-call request id — the Rust
    /// analog of pyfly's `_request_id` context var.
    static REQUEST_ID: String;

    /// Task-local storage slot for the tenant id — the Rust analog of
    /// pyfly's `_tenant_id` context var.
    static TENANT_ID: String;
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

/// Runs `fut` with the given request id in scope — the Rust analog of
/// pyfly's `set_request_id` context var. Scopes nest: an inner scope
/// shadows the outer id.
pub async fn with_request_id<F: Future>(id: impl Into<String>, fut: F) -> F::Output {
    REQUEST_ID.scope(id.into(), fut).await
}

/// Runs the synchronous closure `f` with the given request id in
/// scope. Useful from blocking code and plain `#[test]` functions.
pub fn with_request_id_sync<F: FnOnce() -> R, R>(id: impl Into<String>, f: F) -> R {
    REQUEST_ID.sync_scope(id.into(), f)
}

/// Extracts the request id from the current task-local scope, returning
/// `None` when no scope is active or the id is empty — the Rust analog
/// of pyfly's `get_request_id()`.
pub fn request_id() -> Option<String> {
    REQUEST_ID
        .try_with(Clone::clone)
        .ok()
        .filter(|id| !id.is_empty())
}

/// Returns a 32-character hex-encoded random id suitable for
/// `X-Request-Id` propagation — pyfly generates one per HTTP call when
/// the header is absent.
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Runs `fut` with the given tenant id in scope — the Rust analog of
/// pyfly's `set_tenant_id` context var. Scopes nest: an inner scope
/// shadows the outer id.
pub async fn with_tenant_id<F: Future>(id: impl Into<String>, fut: F) -> F::Output {
    TENANT_ID.scope(id.into(), fut).await
}

/// Runs the synchronous closure `f` with the given tenant id in scope.
/// Useful from blocking code and plain `#[test]` functions.
pub fn with_tenant_id_sync<F: FnOnce() -> R, R>(id: impl Into<String>, f: F) -> R {
    TENANT_ID.sync_scope(id.into(), f)
}

/// Extracts the tenant id from the current task-local scope, returning
/// `None` when no scope is active or the id is empty — the Rust analog
/// of pyfly's `get_tenant_id()`. Tenant ids are never generated
/// server-side: `None` means "unscoped".
pub fn tenant_id() -> Option<String> {
    TENANT_ID
        .try_with(Clone::clone)
        .ok()
        .filter(|id| !id.is_empty())
}
