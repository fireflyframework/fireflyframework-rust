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
//! A service boots from a **single line** over [`FireflyApplication`] — the Rust
//! analog of Spring Boot's `SpringApplication.run(App.class, args)`. It
//! component-scans the DI container, auto-mounts every `#[rest_controller]`,
//! drains the discovered CQRS handlers / event listeners / `#[scheduled]` tasks,
//! auto-discovers security, self-hosts the admin dashboard, serves an
//! auto-generated OpenAPI surface (Swagger UI + ReDoc), and runs the public +
//! management ports with graceful shutdown:
//!
//! ```no_run
//! # async fn demo() -> Result<(), firefly::BoxError> {
//! firefly::FireflyApplication::new("orders").version("1.0.0").run().await
//! # }
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
//! `#[derive(Component)]`, `#[rest_controller]`, … are reachable as
//! `firefly::command_handler` or via `use firefly::prelude::*;`. Generated code
//! references runtime types
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

mod application;
mod context;
pub use application::{AppContext, Bootstrapped, BoxError, FireflyApplication};
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
    // `firefly_aop` re-exports `inventory` (and `async_trait`), so a
    // macro-generated `firefly_aop::inventory::submit!` aspect-discovery thunk
    // and the `#[firefly_aop::async_trait] impl Aspect` resolve through the
    // facade contract without the user crate depending on either directly.
    pub use ::firefly_aop;
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
    pub use ::firefly_transactional;
    // `firefly_validators` re-exports `regex` + `LazyLock` under
    // `bean::__rt`, so a macro-generated `#[validate(pattern = "...")]` check
    // resolves them through the facade contract without the user crate
    // depending on `regex`.
    pub use ::firefly_validators;
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
pub use firefly_aop as aop;
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
pub use firefly_openapi as openapi;
pub use firefly_orchestration as orchestration;
pub use firefly_reactive as reactive;
pub use firefly_resilience as resilience;
pub use firefly_scheduling as scheduling;
pub use firefly_security as security;
pub use firefly_starter_core as starter_core;
pub use firefly_starter_web as starter_web;
pub use firefly_transactional as transactional;
pub use firefly_validators as validators;
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

/// Force-links the inventory of sibling **layer crates** into THIS binary, so a
/// multi-crate service discovers their beans / controllers / handlers.
///
/// Firefly's component discovery — `container.scan()`, `#[rest_controller]`
/// auto-mount, `#[handlers]` / `#[command_handler]` / `#[event_listener]` /
/// `#[scheduled]` draining — is **link-time** via the `inventory` crate: each
/// crate's `inventory::submit!` registrations live in its own object file, and
/// the linker **drops** an object file unless the final binary references a
/// symbol from it. A `Cargo.toml` dependency is **not** a reference. So in a
/// layered app (`-interfaces` / `-models` / `-core` / `-web`) whose `-web`
/// binary merely *depends* on the others, their registrations are dead-stripped
/// and `scan()` / auto-mount silently see nothing from them.
///
/// Invoke this **once at the crate root** of the binary, naming every layer
/// crate that contributes scanned beans / controllers / handlers, so their
/// registrations are linked in and discovered. `extern crate … as _;` is the
/// minimal forcing reference (no name binding, no unused-import warning):
///
/// ```ignore
/// firefly::link!(myapp_core, myapp_models, myapp_interfaces);
///
/// #[tokio::main]
/// async fn main() -> Result<(), firefly::BoxError> {
///     firefly::FireflyApplication::new("myapp").run().await
/// }
/// ```
///
/// Pair it with [`assert_discovered`](crate::assert_discovered) at startup to
/// catch a forgotten crate as a loud failure instead of a silent bean drop.
#[macro_export]
macro_rules! link {
    ($($krate:ident),+ $(,)?) => {
        $( extern crate $krate as _; )+
    };
}

/// Startup guard for [`link!`](crate::link) wiring in a multi-crate service:
/// asserts the framework discovered at least `min_beans` beans (in `container`)
/// and `min_controllers` `#[rest_controller]`s.
///
/// In a layered app a forgotten `firefly::link!` crate is dead-stripped and its
/// beans / controllers silently vanish from discovery (the "6 of 16 beans"
/// symptom). This turns that into a loud panic at startup. Call it right after
/// [`FireflyApplication::bootstrap`](crate::FireflyApplication::bootstrap) with
/// the returned [`Bootstrapped::container`](crate::Bootstrapped::container).
pub fn assert_discovered(
    container: &firefly_container::Container,
    min_beans: usize,
    min_controllers: usize,
) {
    let beans = container.beans().len();
    let controllers = firefly_web::controller_count();
    assert!(
        beans >= min_beans,
        "firefly: discovered {beans} beans but expected at least {min_beans} — a layer crate is \
         likely not force-linked; add it to `firefly::link!(...)` at the binary's crate root"
    );
    assert!(
        controllers >= min_controllers,
        "firefly: discovered {controllers} controllers but expected at least {min_controllers} — a \
         layer crate is likely not force-linked; add it to `firefly::link!(...)`"
    );
}

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
    // ---- Application bootstrap ------------------------------------------
    /// The turnkey `SpringApplication.run` analog + its readiness-hook context.
    pub use crate::{AppContext, FireflyApplication};

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
    /// Saga / workflow / TCC orchestration with compensation — the engine the
    /// declarative `#[saga]` / `#[workflow]` / `#[tcc]` macros build on, plus the
    /// `StepContext` blackboard and the result/policy types.
    pub use firefly_orchestration::{
        CompensationPolicy, Node, Outcome, RetryPolicy, Saga, SagaFailure, SagaStatus, Step,
        StepContext, Tcc, TccParticipant, Workflow,
    };

    // ---- Aspect-oriented advice -----------------------------------------
    /// Declarative aspects (the runtime behind `#[aspect]`): register an aspect
    /// into the process-global registry, run discovery, weave a call through it
    /// with [`advised`](firefly_aop::advised), and the advice surface the
    /// `#[aspect]`-marked methods are written against ([`Aspect`](firefly_aop::Aspect),
    /// [`JoinPoint`](firefly_aop::JoinPoint), [`Proceed`](firefly_aop::Proceed),
    /// [`AdviceFuture`](firefly_aop::AdviceFuture), [`ok`](firefly_aop::ok)) plus
    /// the in-hand [`AspectRegistry`](firefly_aop::AspectRegistry).
    pub use firefly_aop::{
        advised, ok, register_aspect, register_discovered_aspects, AdviceFuture, Aspect,
        AspectRegistry, JoinPoint, Proceed,
    };

    // ---- Transactional / in-process events ------------------------------
    /// The bridge from in-process events to the EDA broker: register the
    /// process broker, forward an in-process event type to it after commit
    /// (Spring-Modulith-style externalization), or publish a payload directly.
    pub use firefly_eda::{externalize_after_commit, publish_to_broker, register_broker};
    /// Publish an in-process domain event, and bind listeners to a
    /// transaction's commit phase — the runtime behind `#[event_listener]` and
    /// `#[transactional_event_listener]`.
    pub use firefly_transactional::{
        publish_event, register_event_listener, LocalTransactionManager, TransactionPhase,
    };

    // ---- Lifecycle ------------------------------------------------------
    /// The application runner and its programmatic shutdown handle.
    pub use firefly_lifecycle::{Application, ShutdownHandle};

    // ---- Core wiring (starter) -----------------------------------------
    /// One-call wiring of the full web-service core.
    pub use firefly_starter_core::{Core, CoreConfig};

    // ---- Web ------------------------------------------------------------
    /// The web result/error types, the RFC 9457 problem-response helper, the
    /// auto-validating extractors (`Valid<T>` for JSON bodies, `ValidPath` /
    /// `ValidQuery` for path/query objects), the `Multipart` form / file-upload
    /// extractor, the `PageRequest` `Pageable` argument resolver, and the
    /// reactive responders that let a controller return `Mono`/`Flux`:
    /// `MonoJson` (Mono to JSON, empty to 404), `NdJson` (Flux to
    /// backpressured `application/x-ndjson`), and `Sse` / `SseEvents` (Flux to
    /// `text/event-stream`).
    pub use firefly_web::{
        problem_response, MonoJson, Multipart, NdJson, PageRequest, Sse, SseEvents, UploadedFile,
        Valid, ValidPath, ValidQuery, WebError, WebResult,
    };

    // ---- Declarative bean validation -----------------------------------
    /// The JSR-380-style `Validate` trait and its violation set — the target
    /// of `#[derive(Validate)]`.
    pub use firefly_validators::bean::{Validate, ValidationError, ValidationErrors};

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
