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

//! # firefly-cqrs
//!
//! The framework's **type-dispatched command/query bus**, plus pluggable
//! middleware for validation, query caching, and any custom
//! cross-cutting concern. Service authors register typed handlers at
//! startup and dispatch through [`Bus::send`] / [`Bus::query`]; the bus
//! matches by [`TypeId`](std::any::TypeId) — the Rust spelling of the Go
//! module's `reflect.Type` registry and the Java `firefly-common-cqrs`
//! class-keyed dispatch.
//!
//! ## Why generics + `TypeId`?
//!
//! The Java original dispatches by class, the .NET port by type, the Go
//! port by `reflect.Type` behind a generic facade. Rust gets the same
//! single dispatch path with zero casts in user code: handlers are
//! registered and invoked fully typed, and only the internal registry is
//! type-erased.
//!
//! ## Optional capabilities
//!
//! Go discovers extra behaviour through optional interfaces
//! (`Validatable`, `Cacheable`). Rust has no runtime interface queries,
//! so they become overridable default methods on the [`Message`] trait —
//! the corresponding middleware ([`ValidationMiddleware`],
//! [`QueryCache::middleware`]) picks them up automatically.
//!
//! ## Quick start
//!
//! ```
//! use std::time::Duration;
//! use firefly_cqrs::{Bus, CqrsError, Message, QueryCache, ValidationMiddleware};
//! use serde::Serialize;
//!
//! #[derive(Clone, Serialize)]
//! struct CreateUser { name: String }
//!
//! impl Message for CreateUser {
//!     fn validate(&self) -> Result<(), CqrsError> {
//!         if self.name.is_empty() {
//!             return Err(CqrsError::validation("name required"));
//!         }
//!         Ok(())
//!     }
//! }
//!
//! #[derive(Clone, Serialize)]
//! struct GetUser { id: String }
//!
//! impl Message for GetUser {
//!     fn cache_ttl(&self) -> Option<Duration> { Some(Duration::from_secs(60)) }
//! }
//!
//! #[derive(Clone, Debug)]
//! struct UserCreated { id: String, name: String }
//!
//! # fn main() {
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let bus = Bus::new();
//! bus.use_middleware(ValidationMiddleware::new());
//! let cache = QueryCache::new();
//! bus.use_middleware(cache.middleware());
//!
//! bus.register(|c: CreateUser| async move {
//!     Ok::<_, CqrsError>(UserCreated { id: "u1".into(), name: c.name })
//! });
//! bus.register(|q: GetUser| async move {
//!     Ok::<_, CqrsError>(UserCreated { id: q.id, name: "alice".into() })
//! });
//!
//! let created: UserCreated = bus.send(CreateUser { name: "alice".into() }).await.unwrap();
//! let view: UserCreated = bus.query(GetUser { id: created.id }).await.unwrap();
//! assert_eq!(view.name, "alice");
//!
//! cache.invalidate_type::<GetUser>(); // after a mutation
//! # });
//! # }
//! ```
//!
//! ## Mental model
//!
//! ```text
//!                               ┌──────────────┐
//!                               │ msg ↦ TypeId  │
//!                               └──────────────┘
//!                                     │
//!                       registered handlers HashMap<TypeId, _>
//!                                     │
//!    middleware chain  ────────────────┘
//!    (use_middleware)
//!    ┌───┐ ┌───┐ ┌───┐
//!    │ V │ │ Q │ │ T │  V = ValidationMiddleware
//!    └───┘ └───┘ └───┘  Q = QueryCache::middleware
//!                        T = your tracing/auth/etc.
//! ```

//! ## pyfly parity
//!
//! On top of the Go-parity surface above, the crate ports pyfly's CQRS
//! layer: an [`AuthorizationMiddleware`] driven by the
//! [`Message::authorize`] hook, an [`ExecutionContext`] (user / tenant /
//! attributes) threaded through dispatch via
//! [`Bus::send_with_context`] and [`Bus::register_with_context`],
//! fluent [`CommandBuilder`] / [`QueryBuilder`] dispatch builders, an
//! [`EdaCacheInvalidationBridge`] evicting [`QueryCache`] entries when
//! events arrive on a [`firefly_eda`] broker, and a
//! [`Bus::handler_names`] listing for the admin actuator.
//!
//! It also ports pyfly's structured validation result types
//! ([`ValidationResult`] / [`ValidationError`] / [`ValidationSeverity`])
//! and the opt-in [`StructuredValidate`] hook. These run *alongside* the
//! terse [`Message::validate`] path: a message can override
//! [`StructuredValidate::validate_structured`] to accumulate multiple
//! field errors and fold them back into the existing
//! [`CqrsError::Validation`] channel via
//! [`ValidationResult::into_cqrs_error`] — so the unchanged
//! [`ValidationMiddleware`] keeps working untouched.
//!
//! ## Domain-event publishing, metrics, and health
//!
//! The crate ports pyfly's `pyfly.cqrs.event` outbound pipeline: a
//! [`CommandEventPublisher`] port with an [`EdaCommandEventPublisher`] over
//! [`firefly_eda::Publisher`] and a [`NoOpEventPublisher`] default, an
//! [`EventFailureStrategy`], and a [`DomainEventMiddleware`] that harvests a
//! command's [`Message::domain_events`] and forwards each to the broker
//! after a successful dispatch (plus
//! [`Bus::send_publishing`] for result-side events) — the Rust spelling of
//! `@publish_domain_event` + `DefaultCommandBus._try_publish_events`.
//!
//! It also ports pyfly's `CqrsMetricsService` as [`CqrsMetrics`] (+
//! [`MetricsMiddleware`]) recording command/query processed / failed /
//! validation-failed counters and a processing-time histogram into a
//! [`firefly_observability::MetricsRegistry`], and pyfly's
//! `CqrsHealthIndicator` as [`CqrsHealthIndicator`] (UP when the bus has at
//! least one registered handler, else UNKNOWN).

//! ## Reactive surface
//!
//! Alongside the async [`Bus::send`] / [`Bus::query`], the bus exposes a
//! **Reactor / WebFlux-style** reactive surface built on
//! [`firefly_reactive`]: [`Bus::send_mono`] and [`Bus::query_mono`] (plus
//! their `*_with_context` overloads) return a
//! [`firefly_reactive::Mono<R>`] instead of awaiting a plain `R`. They run
//! the *same* handler lookup and the *same* validation / authorization /
//! caching middleware chain — only the return surface differs, exactly as
//! a WebFlux reactive command bus hands back a `Mono<R>`. Errors are
//! mapped from [`CqrsError`] to [`firefly_kernel::FireflyError`] via
//! [`cqrs_error_to_firefly`], so a dispatch flows straight into the RFC
//! 7807 problem stack. The surface is strictly additive: the existing
//! async API and wire formats are unchanged.

#![warn(missing_docs)]

mod authorization;
mod bus;
mod cache;
mod context;
mod correlation;
mod discovery;
mod eda_bridge;
mod error;
mod event;
mod fluent;
mod health;
mod metrics;
mod reactive;
mod validation;

// Re-export `inventory` so `#[command_handler]`/`#[query_handler]`-generated
// `HandlerRegistration` thunks submit through `firefly_cqrs::inventory`.
pub use inventory;

pub use authorization::{
    AuthorizationError, AuthorizationMiddleware, AuthorizationResult, AuthorizationSeverity,
    AUTHORIZATION_ERROR_CODE,
};
pub use bus::{
    AnyResult, Bus, DynHandler, Envelope, HandlerFuture, Message, MessageKind, Middleware,
    ValidationMiddleware,
};
pub use cache::{QueryCache, QueryCacheMiddleware};
pub use context::{ExecutionContext, ExecutionContextBuilder};
pub use correlation::CorrelationMiddleware;
pub use discovery::{
    discovered_handler_bean_count, discovered_handler_count, register_discovered_handler_beans,
    register_discovered_handlers, BeanHandlerRegistration, HandlerRegistration,
};
pub use eda_bridge::{
    resolve_pattern, CacheInvalidationEvent, EdaCacheInvalidationBridge, CACHE_INVALIDATION_TOPIC,
};
pub use error::CqrsError;
pub use event::{
    publish_domain_events, CommandEventPublisher, DomainEvent, DomainEventMiddleware, DomainEvents,
    EdaCommandEventPublisher, EventFailureStrategy, NoOpEventPublisher, DEFAULT_EVENT_DESTINATION,
};
pub use fluent::{CommandBuilder, MessageMetadata, QueryBuilder};
pub use health::{CqrsHealthIndicator, CQRS_HEALTH_INDICATOR_NAME};
pub use metrics::{CqrsMetrics, MetricsMiddleware};
pub use reactive::cqrs_error_to_firefly;
pub use validation::{
    StructuredValidate, ValidationError, ValidationResult, ValidationSeverity,
    VALIDATION_ERROR_CODE,
};

/// The released framework version. Calendar-versioned (`YY.M.PATCH`)
/// expressed as valid semver — the Go port's `26.05.01` corresponds to
/// `26.6.15` in the June 2026 release window.
pub const VERSION: &str = "26.6.15";
