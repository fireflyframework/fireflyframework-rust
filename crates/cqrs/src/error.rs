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

//! The error family shared by the bus, its middleware, and handlers.

use thiserror::Error;

use crate::authorization::AuthorizationResult;

/// Errors produced by the CQRS [`Bus`](crate::Bus), its middleware, and
/// the handlers it dispatches to.
///
/// Mirrors the Go module's error surface: the `NoHandler` variant plays
/// the role of Go's `ErrNoHandler` sentinel (test for it with
/// [`CqrsError::is_no_handler`]), the two mismatch variants reproduce the
/// `handler type mismatch` / `result type mismatch` dynamic-dispatch
/// guards, and `Validation` carries a validation failure verbatim — just
/// like Go's validation middleware returns the `Validate()` error
/// unchanged.
#[derive(Debug, Error)]
pub enum CqrsError {
    /// No handler is registered for the dispatched message type.
    ///
    /// The Rust spelling of Go's `ErrNoHandler` sentinel; the display
    /// string carries the unrouted message's type name exactly like Go's
    /// `fmt.Errorf("%w: %T", ErrNoHandler, msg)`.
    #[error("firefly/cqrs: no handler registered: {type_name}")]
    NoHandler {
        /// Fully-qualified Rust type name of the unrouted message.
        type_name: &'static str,
    },

    /// The registered handler received a message of an unexpected type.
    ///
    /// Unreachable through the typed [`Bus::send`](crate::Bus::send) path
    /// (the registry is keyed by [`TypeId`](std::any::TypeId)) but kept
    /// for parity with Go's dynamic-dispatch guard — custom middleware
    /// that swaps envelopes could still trip it.
    #[error("firefly/cqrs: handler type mismatch want {want} got {got}")]
    HandlerTypeMismatch {
        /// Message type the handler was registered for.
        want: &'static str,
        /// Message type that actually arrived.
        got: &'static str,
    },

    /// The handler produced a result of a different type than the caller
    /// asked for — e.g. a handler registered as `CreateUser -> UserCreated`
    /// dispatched via `send::<CreateUser, SomethingElse>`.
    #[error("firefly/cqrs: result type mismatch want {want} got {got}")]
    ResultTypeMismatch {
        /// Result type the caller expected.
        want: &'static str,
        /// Result type the handler actually returned.
        got: &'static str,
    },

    /// Pre-dispatch validation failure raised by
    /// [`Message::validate`](crate::Message::validate) and surfaced by the
    /// [`ValidationMiddleware`](crate::ValidationMiddleware).
    ///
    /// Displays the message verbatim, matching Go where the middleware
    /// returns the `Validate()` error unchanged.
    #[error("{0}")]
    Validation(String),

    /// Pre-dispatch authorization denial raised by
    /// [`Message::authorize`](crate::Message::authorize) and surfaced by
    /// the [`AuthorizationMiddleware`](crate::AuthorizationMiddleware) —
    /// pyfly's `AuthorizationException` (code `AUTHORIZATION_DENIED`).
    ///
    /// Displays the result's summary if set, else the joined
    /// `"<resource>: <message>"` error messages, else
    /// `"Authorization denied"` — pyfly's exception message derivation.
    /// The full [`AuthorizationResult`] (errors, codes, severities,
    /// denied actions) is carried for callers to inspect.
    #[error("{0}")]
    Authorization(AuthorizationResult),

    /// JSON-encoding the message for the query-cache key failed. The
    /// caching middleware treats this as "skip the cache and dispatch",
    /// matching Go's `keyOf` fall-through.
    #[error("firefly/cqrs: cache key serialization failed: {0}")]
    Serialization(String),

    /// Domain failure raised by a handler. Displays the message verbatim
    /// — the analog of a Go handler returning an arbitrary `error`.
    #[error("{0}")]
    Handler(String),

    /// Publishing a domain event to the EDA broker failed — pyfly's
    /// `CommandProcessingException` raised by `_try_publish_events` under
    /// [`EventFailureStrategy::Raise`](crate::EventFailureStrategy::Raise),
    /// or the wrapped transport error surfaced by
    /// [`EdaCommandEventPublisher`](crate::EdaCommandEventPublisher).
    #[error("firefly/cqrs: domain event publish failed: {0}")]
    EventPublish(String),
}

impl CqrsError {
    /// Builds a [`CqrsError::Validation`] — the conventional return value
    /// of a failing [`Message::validate`](crate::Message::validate).
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    /// Builds a [`CqrsError::Handler`] — the conventional failure channel
    /// for domain errors inside a handler.
    pub fn handler(message: impl Into<String>) -> Self {
        Self::Handler(message.into())
    }

    /// Builds a [`CqrsError::Authorization`] from a denial — the Rust
    /// spelling of pyfly raising `AuthorizationException(result)`.
    pub fn authorization(result: AuthorizationResult) -> Self {
        Self::Authorization(result)
    }

    /// Returns `true` when the error is [`CqrsError::NoHandler`] — the
    /// Rust spelling of Go's `errors.Is(err, ErrNoHandler)`.
    pub fn is_no_handler(&self) -> bool {
        matches!(self, Self::NoHandler { .. })
    }

    /// Returns `true` when the error is [`CqrsError::Authorization`] —
    /// the Rust spelling of pyfly's
    /// `isinstance(err, AuthorizationException)`.
    pub fn is_authorization(&self) -> bool {
        matches!(self, Self::Authorization(_))
    }

    /// Borrows the denial behind a [`CqrsError::Authorization`], or
    /// `None` for every other variant — pyfly's `exc.result`.
    pub fn authorization_result(&self) -> Option<&AuthorizationResult> {
        match self {
            Self::Authorization(result) => Some(result),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for CqrsError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err.to_string())
    }
}
