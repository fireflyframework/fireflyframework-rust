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

#![warn(missing_docs)]

mod authorization;
mod bus;
mod cache;
mod context;
mod eda_bridge;
mod error;
mod fluent;

pub use authorization::{
    AuthorizationError, AuthorizationMiddleware, AuthorizationResult, AuthorizationSeverity,
    AUTHORIZATION_ERROR_CODE,
};
pub use bus::{
    AnyResult, Bus, DynHandler, Envelope, HandlerFuture, Message, Middleware, ValidationMiddleware,
};
pub use cache::{QueryCache, QueryCacheMiddleware};
pub use context::{ExecutionContext, ExecutionContextBuilder};
pub use eda_bridge::{
    resolve_pattern, CacheInvalidationEvent, EdaCacheInvalidationBridge, CACHE_INVALIDATION_TOPIC,
};
pub use error::CqrsError;
pub use fluent::{CommandBuilder, MessageMetadata, QueryBuilder};

/// The released framework version. Calendar-versioned (`YY.M.PATCH`)
/// expressed as valid semver — the Go port's `26.05.01` corresponds to
/// `26.6.1` in the June 2026 release window.
pub const VERSION: &str = "26.6.1";
