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

//! # firefly-kernel
//!
//! The shared-vocabulary tier of the Firefly Framework for Rust. It
//! exposes the four primitives every Firefly crate agrees on:
//!
//! 1. The RFC 7807 [`ProblemDetail`] envelope.
//! 2. The [`FireflyResult<T>`] success-or-failure alias.
//! 3. The [`Clock`] abstraction for testable time.
//! 4. The [`FireflyError`] typed error family.
//!
//! Every method in every other crate returns one of these types. The
//! wire shape is identical to the Java firefly-common module, the .NET
//! `FireflyFramework.Kernel` project, and the Go `kernel` module — a
//! service running version `X` on any of the runtimes emits the same
//! JSON.
//!
//! The pyfly-parity layer adds the [`ddd`] module (the zero-dependency
//! DDD kit: [`ddd::Specification`], [`ddd::Entity`],
//! [`ddd::PendingEvents`], [`ddd::TransientDomainEvent`]), the
//! domain-error constructors [`FireflyError::business_rule`] /
//! [`FireflyError::aggregate_not_found`], the request-id /
//! tenant-id task-local scopes alongside the correlation id, and the
//! typed structured-error model [`ErrorResponse`] (with
//! [`ErrorCategory`] / [`ErrorSeverity`] / [`FieldError`]) — additive
//! over [`ProblemDetail`], matching pyfly's `ErrorResponse.to_dict()`
//! shape without changing the Go-parity `problem+json` bytes.
//!
//! ## Why a separate crate?
//!
//! Java's `Throwable` hierarchy and .NET's `Exception` family are
//! stable language fixtures. Rust's [`std::error::Error`] trait is
//! intentionally minimal — which means every framework that wants
//! typed error codes / structured fields / HTTP status mapping has to
//! invent its own. `firefly-kernel` provides the canonical type so the
//! whole platform agrees, and so the wire is identical across runtimes.
//!
//! ## Quick start
//!
//! ```
//! use firefly_kernel::{FireflyError, FireflyResult};
//!
//! fn charge(order_id: &str) -> FireflyResult<()> {
//!     if order_id.is_empty() {
//!         return Err(FireflyError::bad_request("order id required")
//!             .with_field("field", "orderId"));
//!     }
//!     // … domain logic …
//!     Ok(())
//! }
//!
//! let err = charge("").unwrap_err();
//! assert_eq!(err.status, 400);
//! let problem = err.to_problem(); // render RFC 7807
//! assert_eq!(problem.status, 400);
//! ```

mod clock;
mod correlation;
pub mod ddd;
mod error_response;
mod errors;
mod problem;

pub use clock::{Clock, FixedClock, MutableClock, SystemClock};
pub use correlation::{
    correlation_id, new_correlation_id, new_request_id, request_id, tenant_id, with_correlation_id,
    with_correlation_id_sync, with_request_id, with_request_id_sync, with_tenant_id,
    with_tenant_id_sync, HEADER_CORRELATION_ID, HEADER_IDEMPOTENCY_KEY, HEADER_REQUEST_ID,
    HEADER_TENANT_ID,
};
pub use ddd::{
    AndSpec, BoxedDomainEvent, Entity, EventMeta, NotSpec, OrSpec, PendingEvents, Specification,
    TransientDomainEvent,
};
pub use error_response::{ErrorCategory, ErrorResponse, ErrorSeverity, FieldError};
pub use errors::{as_problem, is_firefly, status_of, FireflyError, FireflyResult};
pub use problem::{
    ProblemDetail, PROBLEM_CONTENT_TYPE, TYPE_BAD_REQUEST, TYPE_CONFLICT, TYPE_FORBIDDEN,
    TYPE_IDEMPOTENCY, TYPE_INTERNAL, TYPE_NOT_FOUND, TYPE_RATE_LIMITED, TYPE_UNAUTHORIZED,
    TYPE_UNPROCESSABLE, TYPE_VALIDATION,
};

/// The released framework version. Calendar-versioned (`YY.M.PATCH`)
/// expressed as valid semver — the Go port's `26.05.01` corresponds to
/// `26.6.23` in the June 2026 release window. Embedded in the actuator
/// `/version` payload and the startup banner.
pub const VERSION: &str = "26.6.23";
