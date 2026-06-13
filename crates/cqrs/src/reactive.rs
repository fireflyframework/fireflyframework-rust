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

//! Reactive (Reactor / WebFlux-style) dispatch surface for the [`Bus`].
//!
//! This module is **strictly additive**: it layers a
//! [`firefly_reactive::Mono`] return surface on top of the existing
//! async [`Bus::send`] / [`Bus::query`] without changing their
//! signatures, the registry, the middleware chain, or any wire format.
//! The reactive methods run the *same* handler lookup, validation,
//! authorization, and caching middleware — they only wrap the eventual
//! result in a lazy `Mono`.
//!
//! ## Reactor / WebFlux analogy
//!
//! In Spring WebFlux a reactive command bus hands back a
//! `Mono<R>` instead of a blocking `R`: the work is described now and run
//! on subscription. [`Bus::send_mono`] / [`Bus::query_mono`] are the Rust
//! spelling of exactly that — the imperative `bus.send(cmd).await`
//! becomes `bus.send_mono(cmd)` yielding a `Mono<R>` you compose with
//! Reactor operators (`map`, `flat_map`, `zip_with`, …) and terminate
//! with `.block().await`, `.subscribe(..)`, or `.await`.
//!
//! Because [`firefly_reactive::Mono`] fixes its error to
//! [`firefly_kernel::FireflyError`] (WebFlux models everything as a
//! `Throwable`), the bus's [`CqrsError`] is mapped into a `FireflyError`
//! via [`cqrs_error_to_firefly`] — the same RFC 7807-typed error the rest
//! of the reactive stack (web, data, client) speaks. The mapping is total
//! and status-faithful: a validation failure becomes a 422, an
//! authorization denial a 403, a missing handler / type mismatch /
//! domain error a 500.

use std::sync::Arc;

use firefly_kernel::FireflyError;
use firefly_reactive::Mono;

use crate::bus::{Bus, Message};
use crate::context::ExecutionContext;
use crate::error::CqrsError;

/// Maps a [`CqrsError`] into the [`FireflyError`] the reactive stack
/// speaks, preserving the HTTP status each variant logically maps to.
///
/// `firefly-reactive` fixes its error channel to [`FireflyError`] exactly
/// as Reactor fixes everything to `Throwable`; this is the single bridge
/// that lets a CQRS dispatch flow through a [`Mono`] and out as an RFC
/// 7807 problem response. The original [`CqrsError`] is preserved as the
/// [`std::error::Error::source`] cause so callers (and tests) can still
/// downcast and inspect it.
///
/// | [`CqrsError`] variant   | HTTP status | meaning                          |
/// |-------------------------|-------------|----------------------------------|
/// | [`CqrsError::Validation`]          | 422 | message failed validation        |
/// | [`CqrsError::Authorization`]       | 403 | authorization denied             |
/// | [`CqrsError::NoHandler`]           | 500 | no handler registered            |
/// | [`CqrsError::HandlerTypeMismatch`] | 500 | dispatch invariant broken        |
/// | [`CqrsError::ResultTypeMismatch`]  | 500 | dispatch invariant broken        |
/// | [`CqrsError::Serialization`]       | 500 | cache-key encoding failed        |
/// | [`CqrsError::Handler`]             | 500 | domain failure inside a handler  |
/// | [`CqrsError::EventPublish`]        | 500 | domain-event publish failed      |
#[must_use]
pub fn cqrs_error_to_firefly(err: CqrsError) -> FireflyError {
    let detail = err.to_string();
    let firefly = match &err {
        CqrsError::Validation(_) => FireflyError::validation(detail),
        CqrsError::Authorization(_) => FireflyError::forbidden(detail),
        CqrsError::NoHandler { .. }
        | CqrsError::HandlerTypeMismatch { .. }
        | CqrsError::ResultTypeMismatch { .. }
        | CqrsError::Serialization(_)
        | CqrsError::Handler(_)
        | CqrsError::EventPublish(_) => FireflyError::internal(detail),
    };
    firefly.with_cause(err)
}

impl Bus {
    /// Dispatches a command and returns its result as a
    /// [`firefly_reactive::Mono`] — the reactive twin of [`Bus::send`].
    ///
    /// ## Reactor / WebFlux analogy
    ///
    /// This is the Rust spelling of a WebFlux reactive command bus
    /// returning `Mono<R>`: nothing runs until the `Mono` is subscribed,
    /// blocked, or awaited, at which point it executes the *same* handler
    /// lookup and the *same* validation / authorization / caching
    /// middleware chain as [`Bus::send`] — only the return surface
    /// differs. Compose it with Reactor operators and terminate it with
    /// `.block().await`, `.subscribe(..)`, or `.await`.
    ///
    /// Errors are mapped from [`CqrsError`] to
    /// [`firefly_kernel::FireflyError`] via [`cqrs_error_to_firefly`]
    /// (so a no-handler dispatch surfaces as a 500 problem, a validation
    /// failure as a 422, an authorization denial as a 403), with the
    /// original [`CqrsError`] preserved as the error's
    /// [`std::error::Error::source`].
    ///
    /// ```
    /// use std::sync::Arc;
    /// use firefly_cqrs::{Bus, CqrsError, Message};
    /// use serde::Serialize;
    ///
    /// #[derive(Clone, Serialize)]
    /// struct CreateUser { name: String }
    /// impl Message for CreateUser {}
    ///
    /// #[derive(Clone)]
    /// struct UserCreated { id: String }
    ///
    /// # fn main() {
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
    /// let bus = Arc::new(Bus::new());
    /// bus.register(|c: CreateUser| async move {
    ///     Ok::<_, CqrsError>(UserCreated { id: format!("u-{}", c.name) })
    /// });
    ///
    /// let created = bus
    ///     .send_mono::<_, UserCreated>(CreateUser { name: "alice".into() })
    ///     .map(|u| u.id)
    ///     .block()
    ///     .await
    ///     .unwrap();
    /// assert_eq!(created, Some("u-alice".to_string()));
    /// # });
    /// # }
    /// ```
    #[must_use = "a Mono is lazy and does nothing unless subscribed, blocked, or awaited"]
    pub fn send_mono<C, R>(self: &Arc<Self>, command: C) -> Mono<R>
    where
        C: Message,
        R: Clone + Send + Sync + 'static,
    {
        let bus = Arc::clone(self);
        Mono::from_result_future(async move {
            bus.send::<C, R>(command)
                .await
                .map_err(cqrs_error_to_firefly)
        })
    }

    /// Dispatches a query and returns its result as a
    /// [`firefly_reactive::Mono`] — the reactive twin of [`Bus::query`]
    /// (itself a synonym for [`Bus::send`]).
    ///
    /// ## Reactor / WebFlux analogy
    ///
    /// The read-side companion of [`Bus::send_mono`]: a WebFlux reactive
    /// query bus handing back `Mono<R>`. It runs the identical middleware
    /// chain (including the query cache) and maps errors through
    /// [`cqrs_error_to_firefly`]. Lazy until subscribed/blocked/awaited.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::time::Duration;
    /// use firefly_cqrs::{Bus, CqrsError, Message};
    /// use serde::Serialize;
    ///
    /// #[derive(Clone, Serialize)]
    /// struct GetUser { id: String }
    /// impl Message for GetUser {
    ///     fn cache_ttl(&self) -> Option<Duration> { Some(Duration::from_secs(60)) }
    /// }
    ///
    /// #[derive(Clone)]
    /// struct UserView { name: String }
    ///
    /// # fn main() {
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
    /// let bus = Arc::new(Bus::new());
    /// bus.register(|q: GetUser| async move {
    ///     Ok::<_, CqrsError>(UserView { name: format!("user-{}", q.id) })
    /// });
    ///
    /// let name = bus
    ///     .query_mono::<_, UserView>(GetUser { id: "42".into() })
    ///     .map(|v| v.name)
    ///     .block()
    ///     .await
    ///     .unwrap();
    /// assert_eq!(name, Some("user-42".to_string()));
    /// # });
    /// # }
    /// ```
    #[must_use = "a Mono is lazy and does nothing unless subscribed, blocked, or awaited"]
    pub fn query_mono<Q, R>(self: &Arc<Self>, query: Q) -> Mono<R>
    where
        Q: Message,
        R: Clone + Send + Sync + 'static,
    {
        self.send_mono::<Q, R>(query)
    }

    /// [`Bus::send_mono`] with an [`ExecutionContext`] attached — the
    /// reactive twin of [`Bus::send_with_context`].
    ///
    /// ## Reactor / WebFlux analogy
    ///
    /// The context-carrying overload, mirroring how a WebFlux handler
    /// threads request-scoped state (the authenticated principal, tenant,
    /// attributes) into the reactive pipeline. The context reaches
    /// [`Message::authorize`](crate::Message::authorize), any middleware
    /// reading [`Envelope::context`](crate::Envelope::context), and
    /// handlers registered via
    /// [`Bus::register_with_context`](crate::Bus::register_with_context).
    #[must_use = "a Mono is lazy and does nothing unless subscribed, blocked, or awaited"]
    pub fn send_mono_with_context<C, R>(
        self: &Arc<Self>,
        command: C,
        context: ExecutionContext,
    ) -> Mono<R>
    where
        C: Message,
        R: Clone + Send + Sync + 'static,
    {
        let bus = Arc::clone(self);
        Mono::from_result_future(async move {
            bus.send_with_context::<C, R>(command, context)
                .await
                .map_err(cqrs_error_to_firefly)
        })
    }

    /// [`Bus::query_mono`] with an [`ExecutionContext`] attached — the
    /// reactive twin of [`Bus::query_with_context`].
    ///
    /// ## Reactor / WebFlux analogy
    ///
    /// The read-side companion of [`Bus::send_mono_with_context`].
    #[must_use = "a Mono is lazy and does nothing unless subscribed, blocked, or awaited"]
    pub fn query_mono_with_context<Q, R>(
        self: &Arc<Self>,
        query: Q,
        context: ExecutionContext,
    ) -> Mono<R>
    where
        Q: Message,
        R: Clone + Send + Sync + 'static,
    {
        self.send_mono_with_context::<Q, R>(query, context)
    }
}
