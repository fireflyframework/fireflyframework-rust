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

//! # Firefly — the one-dependency front door to the framework
//!
//! `firefly` is the Spring-Boot-starter developer experience for the Firefly
//! Framework: add **one** dependency, glob-import **one** prelude, and you have
//! the whole framework — CQRS, the dependency-injection container, the reactive
//! web stack, event-driven messaging, scheduling, saga/TCC/workflow
//! orchestration, resilience, security, observability, lifecycle, and the
//! declarative `#[derive(...)]` / `#[...]` macro layer — all under stable paths.
//!
//! Without this crate a service had to list ten-to-fifteen `firefly-*` crates in
//! its `Cargo.toml` and import from each. With it:
//!
//! ```toml
//! [dependencies]
//! firefly = "26.6"
//! ```
//!
//! ```no_run
//! use firefly::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> FireflyResult<()> {
//!     let core = Core::new(CoreConfig {
//!         app_name: "orders".into(),
//!         app_version: "1.0.0".into(),
//!         ..CoreConfig::default()
//!     });
//!     core.print_banner();
//!     Ok(())
//! }
//! ```
//!
//! ## Three ways in
//!
//! | You write | You get |
//! |-----------|---------|
//! | [`use firefly::prelude::*;`](prelude) | The high-frequency surface: [`Bus`](prelude::Bus), [`Container`](prelude::Container), [`Scheduler`](prelude::Scheduler), [`Saga`](prelude::Saga), [`Core`](prelude::Core), every macro, … |
//! | `firefly::cqrs::…`, `firefly::web::…` | Ergonomic per-crate aliases — `firefly::cqrs::Bus` instead of `firefly_cqrs::Bus` |
//! | `firefly::__rt::firefly_cqrs::…` | The hidden, **stable** path that macro-generated code targets (see [`__rt`]) |
//!
//! ## Macros
//!
//! Every macro from [`firefly-macros`](firefly_macros) is re-exported at the
//! crate root *and* in the [`prelude`], so `#[command_handler]`, `#[scheduled]`,
//! `#[derive(Component)]`, `#[saga]`, … are reachable as `firefly::command_handler`
//! or via `use firefly::prelude::*;`. Generated code references runtime types
//! through the [`__rt`] contract path, so a user only ever needs the single
//! `firefly` dependency.
//!
//! ## Staying lean
//!
//! The default build pulls only the framework's *port* crates (no heavy
//! third-party drivers). Heavy adapters are opt-in cargo features:
//!
//! ```toml
//! firefly = { version = "26.6", features = ["data-sqlx", "eda-kafka"] }
//! ```
//!
//! | Feature | Re-exports under | Pulls in |
//! |---------|------------------|----------|
//! | `data-sqlx` | [`firefly::data_sqlx`](crate#reexports) | relational repository adapter (pg/mysql/sqlite) |
//! | `data-mongodb` | `firefly::data_mongodb` | document repository adapter |
//! | `eda-kafka` | `firefly::eda_kafka` | Kafka broker |
//! | `eda-rabbitmq` | `firefly::eda_rabbitmq` | RabbitMQ broker |
//! | `eda-redis` | `firefly::eda_redis` | Redis Streams broker |
//! | `eda-postgres` | `firefly::eda_postgres` | Postgres broker |
//! | `cache-redis` | `firefly::cache_redis` | Redis cache adapter |
//! | `cache-postgres` | `firefly::cache_postgres` | Postgres cache adapter |
//! | `admin` | `firefly::admin` | back-office / admin surface |
//! | `full` | all of the above | — |
//!
//! A minimal `firefly` dependency compiles none of them.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]

mod context;
pub use context::{ApplicationContext, ApplicationContextBuilder};

/// Component-scan a container: register every stereotype-annotated bean
/// discovered across the crate graph, honoring conditionals/profiles.
///
/// The free-function form of
/// [`Container::scan`](firefly_container::Container::scan), so
/// `firefly::scan(&container)` reads as the pyfly `scan_package` analog.
/// Returns the number of beans registered.
pub fn scan(container: &firefly_container::Container) -> usize {
    container.scan()
}

// ---------------------------------------------------------------------------
// 1. The `__rt` contract: EVERY runtime crate re-exported under its crate name.
// ---------------------------------------------------------------------------

/// **Macro contract path — not part of the public API.**
///
/// `firefly-macros`-generated code references runtime types through this one
/// stable module — e.g. `::firefly::__rt::firefly_cqrs::Bus` — so a user who
/// depends only on the `firefly` facade never has to add the underlying
/// `firefly-*` crates to reference what a macro expands to.
///
/// Each item is re-exported under **exactly its crate name** (the crate name
/// with the `firefly_` prefix). These names are a guarantee: the macro crate
/// generates against them, so they must not be renamed.
///
/// You should never write `firefly::__rt::…` by hand; use the [`prelude`] or
/// the ergonomic aliases ([`cqrs`], [`web`], …) instead.
#[doc(hidden)]
pub mod __rt {
    pub use ::firefly_actuator;
    pub use ::firefly_cache;
    pub use ::firefly_client;
    pub use ::firefly_config;
    // `firefly_container` re-exports `inventory`, so macro-generated
    // `firefly_container::inventory::submit!` thunks (component scan + route
    // metadata) resolve through the facade contract without the user crate
    // depending on `inventory` directly.
    pub use ::firefly_container;
    pub use ::firefly_cqrs;
    pub use ::firefly_data;
    pub use ::firefly_eda;
    pub use ::firefly_eventsourcing;
    pub use ::firefly_kernel;
    pub use ::firefly_lifecycle;
    pub use ::firefly_observability;
    pub use ::firefly_orchestration;
    pub use ::firefly_reactive;
    pub use ::firefly_resilience;
    pub use ::firefly_scheduling;
    pub use ::firefly_security;
    pub use ::firefly_starter_core;
    pub use ::firefly_starter_web;
    pub use ::firefly_web;

    // Third-party crate re-exported under the same contract so generated code
    // never forces the user crate to add it directly. `#[derive(DomainEvent)]`
    // JSON-encodes the event payload through `::firefly::__rt::serde_json`,
    // keeping the one-dependency facade promise intact for users who depend on
    // only `firefly` + `serde`.
    pub use ::serde_json;

    // Optional adapter crates — present in `__rt` only when their feature is on,
    // so generated code that targets an adapter still resolves through the same
    // contract path.
    #[cfg(feature = "admin")]
    pub use ::firefly_admin;
    #[cfg(feature = "cache-postgres")]
    pub use ::firefly_cache_postgres;
    #[cfg(feature = "cache-redis")]
    pub use ::firefly_cache_redis;
    #[cfg(feature = "data-mongodb")]
    pub use ::firefly_data_mongodb;
    #[cfg(feature = "data-sqlx")]
    pub use ::firefly_data_sqlx;
    #[cfg(feature = "eda-kafka")]
    pub use ::firefly_eda_kafka;
    #[cfg(feature = "eda-postgres")]
    pub use ::firefly_eda_postgres;
    #[cfg(feature = "eda-rabbitmq")]
    pub use ::firefly_eda_rabbitmq;
    #[cfg(feature = "eda-redis")]
    pub use ::firefly_eda_redis;
}

// ---------------------------------------------------------------------------
// 2. Ergonomic module aliases: `firefly::cqrs::…` for every runtime crate.
// ---------------------------------------------------------------------------

pub use firefly_actuator as actuator;
pub use firefly_cache as cache;
pub use firefly_client as client;
pub use firefly_config as config;
pub use firefly_container as container;
pub use firefly_cqrs as cqrs;
pub use firefly_data as data;
pub use firefly_eda as eda;
pub use firefly_eventsourcing as eventsourcing;
pub use firefly_kernel as kernel;
pub use firefly_lifecycle as lifecycle;
pub use firefly_observability as observability;
pub use firefly_orchestration as orchestration;
pub use firefly_reactive as reactive;
pub use firefly_resilience as resilience;
pub use firefly_scheduling as scheduling;
pub use firefly_security as security;
pub use firefly_starter_core as starter_core;
pub use firefly_starter_web as starter_web;
pub use firefly_web as web;

// Optional adapter aliases (feature-gated).
#[cfg(feature = "admin")]
#[cfg_attr(docsrs, doc(cfg(feature = "admin")))]
pub use firefly_admin as admin;
#[cfg(feature = "cache-postgres")]
#[cfg_attr(docsrs, doc(cfg(feature = "cache-postgres")))]
pub use firefly_cache_postgres as cache_postgres;
#[cfg(feature = "cache-redis")]
#[cfg_attr(docsrs, doc(cfg(feature = "cache-redis")))]
pub use firefly_cache_redis as cache_redis;
#[cfg(feature = "data-mongodb")]
#[cfg_attr(docsrs, doc(cfg(feature = "data-mongodb")))]
pub use firefly_data_mongodb as data_mongodb;
#[cfg(feature = "data-sqlx")]
#[cfg_attr(docsrs, doc(cfg(feature = "data-sqlx")))]
pub use firefly_data_sqlx as data_sqlx;
#[cfg(feature = "eda-kafka")]
#[cfg_attr(docsrs, doc(cfg(feature = "eda-kafka")))]
pub use firefly_eda_kafka as eda_kafka;
#[cfg(feature = "eda-postgres")]
#[cfg_attr(docsrs, doc(cfg(feature = "eda-postgres")))]
pub use firefly_eda_postgres as eda_postgres;
#[cfg(feature = "eda-rabbitmq")]
#[cfg_attr(docsrs, doc(cfg(feature = "eda-rabbitmq")))]
pub use firefly_eda_rabbitmq as eda_rabbitmq;
#[cfg(feature = "eda-redis")]
#[cfg_attr(docsrs, doc(cfg(feature = "eda-redis")))]
pub use firefly_eda_redis as eda_redis;

// ---------------------------------------------------------------------------
// 3. Macros at the crate root: `firefly::command_handler`, etc.
// ---------------------------------------------------------------------------

// `allow(unused_imports)`: the glob is empty while `firefly-macros` is a
// placeholder that exports no macros yet. Once the macro crate ships its
// derive/attribute macros, the glob becomes live; the allow keeps the facade
// green under `-D warnings` in both states without churning this line.
#[doc(inline)]
#[allow(unused_imports)]
pub use firefly_macros::*;

// ---------------------------------------------------------------------------
// 4. The prelude — the high-frequency surface plus every macro.
// ---------------------------------------------------------------------------

/// The high-frequency surface, designed for `use firefly::prelude::*;`.
///
/// Re-exports the types you reach for in nearly every service plus **all**
/// macros, so a single glob import gives you CQRS, dependency injection,
/// scheduling, orchestration, the reactive primitives, the web result type,
/// the core wiring struct, and the framework error type.
///
/// ```
/// use firefly::prelude::*;
///
/// // The common types resolve through the glob:
/// let _bus: fn() -> Bus = Bus::new;
/// let _container: fn() -> Container = Container::new;
/// let _ok: FireflyResult<()> = Ok(());
/// ```
pub mod prelude {
    // ---- CQRS -----------------------------------------------------------
    /// The in-process command/query bus.
    pub use firefly_cqrs::{Bus, CqrsError, Message};

    // ---- Dependency-injection container --------------------------------
    /// The `ApplicationContext` orchestrator (scan + conditions + lifecycle).
    pub use crate::ApplicationContext;
    /// The DI container, its bean scopes, deferred [`Provider`], and the
    /// scan/condition surface.
    pub use firefly_container::{ConditionContext, Container, Provider, Scope};

    // ---- Scheduling -----------------------------------------------------
    /// The task scheduler (cron / fixed-rate / fixed-delay).
    pub use firefly_scheduling::Scheduler;

    // ---- Orchestration --------------------------------------------------
    /// Saga orchestration with compensation.
    pub use firefly_orchestration::{Saga, Step};

    // ---- Lifecycle ------------------------------------------------------
    /// The application runner and its programmatic shutdown handle.
    pub use firefly_lifecycle::{Application, ShutdownHandle};

    // ---- Core wiring (starter) -----------------------------------------
    /// One-call wiring of the full web-service core.
    pub use firefly_starter_core::{Core, CoreConfig};

    // ---- Web ------------------------------------------------------------
    /// The web result/error types and the RFC 9457 problem-response helper.
    pub use firefly_web::{problem_response, WebError, WebResult};

    // ---- Kernel errors --------------------------------------------------
    /// The framework-wide error and result types.
    pub use firefly_kernel::{FireflyError, FireflyResult};

    // ---- Reactive primitives -------------------------------------------
    /// The reactive `Mono`/`Flux` types (Reactor-style).
    pub use firefly_reactive::{Flux, Mono};

    // ---- Every macro ----------------------------------------------------
    /// All derive/attribute macros from `firefly-macros`.
    // `allow(unused_imports)`: empty while the macro crate is a placeholder
    // (see the crate-root re-export for the rationale).
    #[allow(unused_imports)]
    pub use firefly_macros::*;
}

/// The released framework version, calendar-versioned (`YY.M.PATCH`) and
/// expressed as valid semver. Matches every other `firefly-*` crate's
/// `VERSION` const.
pub const VERSION: &str = firefly_kernel::VERSION;
