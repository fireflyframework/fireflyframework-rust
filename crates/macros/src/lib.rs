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

//! # firefly-macros — the declarative service-development layer
//!
//! The framework's answer to Spring/pyfly decorators: a set of `#[derive(...)]`
//! and `#[...]` attribute macros that collapse the framework's
//! closure/builder wiring into declarations next to the code they describe.
//!
//! This is a `proc-macro` crate, so it can re-export nothing at runtime.
//! Generated code therefore references every runtime type through the
//! **`firefly` facade's `__rt` contract path** — e.g.
//! `::firefly::__rt::firefly_cqrs::Bus` — so a service that depends only on the
//! `firefly` facade compiles whatever a macro expands to without listing the
//! underlying `firefly-*` crates. Every macro also accepts a
//! `#[firefly(crate = "...")]` argument to override the leading facade segment
//! for users who rename or shim the facade.
//!
//! ## The macros
//!
//! | Macro | On | Generates |
//! |-------|----|-----------|
//! | [`macro@Command`] / [`macro@Query`] (derive) | a message struct | `impl firefly_cqrs::Message` (field `#[firefly(validate)]`, type `#[firefly(cache_ttl = "...")]`) |
//! | [`macro@command_handler`] / [`macro@query_handler`] | an `async fn(Msg) -> Result<R, CqrsError>` | a `register_<fn>(bus)` helper |
//! | [`macro@Component`] / [`macro@Service`] / [`macro@Repository`] (derive) | a struct with `#[autowired]` fields | a `firefly_register(container)` method |
//! | [`register_all!`] | `(container, [A, B, ...])` | calls each type's `firefly_register` |
//! | [`macro@scheduled`] | a zero-arg `async fn` | a `schedule_<fn>(scheduler)` helper |
//! | [`macro@rest_controller`] | an `impl` block (`#[get]`/`#[post]`/… methods) | a `routes(state) -> axum::Router` |
//! | [`macro@DomainEvent`] / [`macro@AggregateRoot`] (derive) | a struct | event-type/aggregate ergonomics |
//! | [`macro@event_listener`] | an `async fn(Event) -> FireflyResult<()>` | a `subscribe_<fn>(broker)` helper |
//!
//! See each macro's own documentation for the argument surface and an example.
//! These are normally reached through the `firefly` facade
//! (`use firefly::prelude::*;`), which re-exports every macro at its root.

#![forbid(unsafe_code)]

mod bean;
mod common;
mod config_properties;
mod container;
mod cqrs;
mod eda;
mod eventsourcing;
mod scheduling;
mod web;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput, ItemFn, ItemImpl};

use crate::container::{RegisterAllInput, Stereotype};

/// Expands a `syn::Result<TokenStream>` into a `proc_macro::TokenStream`,
/// turning an error into a `compile_error!` so diagnostics point at the source.
fn emit(result: syn::Result<proc_macro2::TokenStream>) -> TokenStream {
    match result {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

// ===========================================================================
// CQRS
// ===========================================================================

/// Derives `firefly_cqrs::Message` for a command struct.
///
/// Field- and type-level `#[firefly(...)]` attributes shape the generated
/// `Message` impl:
/// - `#[firefly(validate)]` on a field emits a "required / non-default" check
///   inside `Message::validate` (the field type must be `Default + PartialEq`);
/// - `#[firefly(cache_ttl = "60s")]` on the struct sets `Message::cache_ttl`.
///
/// ```ignore
/// use firefly::prelude::*;
/// use serde::Serialize;
///
/// #[derive(Clone, Serialize, Command)]
/// struct CreateUser {
///     #[firefly(validate)]
///     name: String,
/// }
/// ```
#[proc_macro_derive(Command, attributes(firefly))]
pub fn derive_command(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(cqrs::derive_message(input, false))
}

/// Derives `firefly_cqrs::Message` for a query struct.
///
/// Identical to [`macro@Command`] but conventionally used for read messages;
/// pair it with `#[firefly(cache_ttl = "...")]` to memoise query results
/// through `firefly_cqrs::QueryCache`.
///
/// ```ignore
/// use firefly::prelude::*;
/// use serde::Serialize;
///
/// #[derive(Clone, Serialize, Query)]
/// #[firefly(cache_ttl = "30s")]
/// struct GetUser { id: String }
/// ```
#[proc_macro_derive(Query, attributes(firefly))]
pub fn derive_query(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(cqrs::derive_message(input, true))
}

/// Marks an `async fn(Msg) -> Result<R, CqrsError>` as a command handler and
/// generates a `register_<fn>(bus)` helper that installs it on a
/// `firefly_cqrs::Bus`.
///
/// ```ignore
/// #[command_handler]
/// async fn handle_create_user(cmd: CreateUser) -> Result<UserCreated, CqrsError> {
///     Ok(UserCreated { /* … */ })
/// }
/// // generated: fn register_handle_create_user(bus: &Bus)
/// ```
///
/// Accepts `#[command_handler(crate = "...", register = "...")]`.
#[proc_macro_attribute]
pub fn command_handler(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(cqrs::handler_attr(args.into(), item, false))
}

/// Marks an `async fn(Msg) -> Result<R, CqrsError>` as a query handler and
/// generates a `register_<fn>(bus)` helper. Behaves like
/// [`macro@command_handler`]; the distinct name documents read intent.
///
/// Accepts `#[query_handler(crate = "...", register = "...")]`.
#[proc_macro_attribute]
pub fn query_handler(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(cqrs::handler_attr(args.into(), item, true))
}

// ===========================================================================
// Dependency injection
// ===========================================================================

/// Derives a DI registration for a managed component.
///
/// Generates a `firefly_register(container)` method that registers the type on
/// a `firefly_container::Container`, injecting each field, *and* submits an
/// `inventory` thunk so [`Container::scan()`](firefly_container) discovers the
/// type across the whole crate graph (the Rust analog of pyfly's
/// `scan_package`).
///
/// ## Field injection
/// - `#[autowired]` resolves the field from the container. The field type
///   selects the form: `Arc<T>` → `resolve::<T>()`, `Vec<Arc<T>>` →
///   `resolve_all::<T>()`, `Option<Arc<T>>` → `resolve(..).ok()` (the
///   `required=false` analog), `Provider<T>` → a deferred `provider::<T>()`.
/// - `#[firefly(qualifier = "name")]` resolves a specific named bean.
/// - `#[firefly(value = "${key:default}")]` injects a config value (parsed via
///   `FromStr`).
/// - any other field is built from `Default`.
///
/// ## Type-level `#[firefly(...)]` options
/// `scope = "singleton" | "transient" | "request" | "session"`, `name = "..."`,
/// `primary`, `order = N`, `lazy`, `profile = "expr"`,
/// `condition_on_property = "k=v"`, `condition_on_class = "label"`,
/// `condition_on_bean = "Type"`, `condition_on_missing_bean = "Type"`,
/// `condition_on_single_candidate = "Type"`, `provides = "dyn Port"`
/// (auto-binds the trait object), and lifecycle hooks
/// `post_construct = "method"` / `pre_destroy = "method"`.
///
/// ```ignore
/// #[derive(Component)]
/// #[firefly(scope = "singleton", profile = "prod", provides = "dyn Notifier")]
/// struct OrderService {
///     #[autowired]
///     repo: Arc<OrderRepository>,
///     #[firefly(value = "${order.batch:50}")]
///     batch: usize,
/// }
/// ```
#[proc_macro_derive(Component, attributes(firefly, autowired))]
pub fn derive_component(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(container::derive_component(input, Stereotype::Component))
}

/// Derives a DI registration for a service-layer bean — a [`macro@Component`]
/// alias documenting business-logic intent. Same options.
#[proc_macro_derive(Service, attributes(firefly, autowired))]
pub fn derive_service(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(container::derive_component(input, Stereotype::Service))
}

/// Derives a DI registration for a repository-layer bean — a [`macro@Component`]
/// alias documenting data-access intent. Same options.
#[proc_macro_derive(Repository, attributes(firefly, autowired))]
pub fn derive_repository(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(container::derive_component(input, Stereotype::Repository))
}

/// Derives a DI registration for a configuration-holder bean — a
/// [`macro@Component`] alias whose role is to hold `#[bean]` factory methods
/// (Spring/pyfly `@Configuration`). Pair it with `#[bean]` on an `impl` block.
///
/// ```ignore
/// #[derive(Configuration)]
/// struct AppConfig;
///
/// #[firefly::bean]
/// impl AppConfig {
///     #[bean]
///     fn clock(&self) -> SystemClock { SystemClock::new() }
/// }
/// ```
#[proc_macro_derive(Configuration, attributes(firefly, autowired))]
pub fn derive_configuration(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(container::derive_component(
        input,
        Stereotype::Configuration,
    ))
}

/// Derives a DI registration for a controller bean — a [`macro@Component`]
/// alias documenting web-controller intent (distinct from the `#[rest_controller]`
/// *routing* attribute, which wires axum routes).
#[proc_macro_derive(Controller, attributes(firefly, autowired))]
pub fn derive_controller(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(container::derive_component(input, Stereotype::Controller))
}

/// Derives a config-bound injectable bean (Spring `@ConfigurationProperties` /
/// `@EnableConfigurationProperties`).
///
/// Binds a `serde::Deserialize` struct from the container's active config under
/// a key prefix and registers it as a resolvable singleton, so a component can
/// `#[autowired]` it by type.
///
/// ```ignore
/// #[derive(Deserialize, ConfigProperties)]
/// #[firefly(prefix = "app.db")]
/// struct DbProperties {
///     url: String,
///     pool_size: u32,
/// }
/// ```
#[proc_macro_derive(ConfigProperties, attributes(firefly))]
pub fn derive_config_properties(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(config_properties::derive_config_properties(input))
}

/// Marks the factory methods of a `#[derive(Configuration)]` type as beans
/// (Spring/pyfly `@Bean`).
///
/// Applied to an `impl` block; every method inside carrying a `#[bean(...)]`
/// marker becomes a bean factory keyed by its return type. Method arguments are
/// resolved from the container (`Arc<Dep>`), so a `@bean` method can depend on
/// other beans. Generates `firefly_register_beans(&Container)`.
///
/// Per-method options: `#[bean(name = "...", scope = "...", primary, profile = "...")]`
/// (`profile` gates the bean on the active profiles, Spring `@Bean @Profile`).
///
/// ```ignore
/// #[firefly::bean]
/// impl AppConfig {
///     #[bean(primary)]
///     fn repo(&self, db: Arc<Db>) -> SqlRepo { SqlRepo::new(db) }
/// }
/// // generated: fn AppConfig::firefly_register_beans(&Container)
/// ```
///
/// A `#[bean]` method returns a **concrete (sized) type** — that is the bean's
/// key. Expose it behind a trait via the holder's `#[firefly(provides = ...)]`
/// or a `Container::bind` after registration.
#[proc_macro_attribute]
pub fn bean(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    emit(bean::bean_impl(args.into(), item))
}

/// Registers every listed stereotype type on a container in order.
///
/// The explicit-list fallback to [`Container::scan()`](firefly_container) for
/// **generic** beans (which cannot be inventoried) — calls each type's
/// generated `firefly_register`:
///
/// ```ignore
/// firefly::register_all!(&container, [OrderRepository, OrderService]);
/// ```
#[proc_macro]
pub fn register_all(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as RegisterAllInput);
    container::register_all(input).into()
}

// ===========================================================================
// Scheduling
// ===========================================================================

/// Schedules a zero-argument `async fn`, generating a `schedule_<fn>(scheduler)`
/// helper that registers it on a `firefly_scheduling::Scheduler`.
///
/// Exactly one trigger must be given (a compile error otherwise):
/// `#[scheduled(cron = "0 2 * * *", zone = "America/New_York")]`,
/// `#[scheduled(fixed_rate = "30s", initial_delay = "5s")]`, or
/// `#[scheduled(fixed_delay = "10s")]`.
///
/// ```ignore
/// #[scheduled(fixed_rate = "30s")]
/// async fn flush_metrics() -> Result<(), std::io::Error> { Ok(()) }
/// // generated: fn schedule_flush_metrics(scheduler: &Scheduler)
/// ```
#[proc_macro_attribute]
pub fn scheduled(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(scheduling::scheduled_attr(args.into(), item))
}

// ===========================================================================
// Web
// ===========================================================================

/// Turns an `impl` block into an axum controller, generating a
/// `routes(state) -> axum::Router`.
///
/// Methods carry `#[get("/:id")]` / `#[post]` / `#[put]` / `#[delete]` /
/// `#[patch]`; their signatures use ordinary axum extractors
/// (`axum::extract::{State, Path, Json, Query}`, …) and return
/// `firefly_web::WebResult<T>` so errors render as RFC 7807 problems.
///
/// ```ignore
/// #[rest_controller(path = "/api/v1/orders")]
/// impl OrderApi {
///     #[get("/:id")]
///     async fn get_order(State(api): State<OrderApi>, Path(id): Path<String>)
///         -> WebResult<Json<OrderView>> { /* … */ }
/// }
/// // generated: fn OrderApi::routes(state: OrderApi) -> axum::Router
/// ```
///
/// Accepts `#[rest_controller(path = "...", state = "MyState", crate = "...")]`.
/// The `state` type defaults to the controller (`Self`).
#[proc_macro_attribute]
pub fn rest_controller(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    emit(web::rest_controller(args.into(), item))
}

// ===========================================================================
// Event sourcing
// ===========================================================================

/// Adds domain-event ergonomics to a `Serialize` payload struct: an
/// `EVENT_TYPE` const + `event_type()` accessor and a
/// `to_domain_event(aggregate_id, aggregate_type, version)` conversion onto
/// `firefly_eventsourcing::DomainEvent`.
///
/// Override the discriminator with `#[firefly(event_type = "...")]`.
#[proc_macro_derive(DomainEvent, attributes(firefly))]
pub fn derive_domain_event(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(eventsourcing::derive_domain_event(input))
}

/// Adds aggregate ergonomics to a struct that embeds a
/// `firefly_eventsourcing::AggregateRoot` (field `root` by default): an
/// `AGGREGATE_TYPE` const and `aggregate()` / `aggregate_mut()` accessors.
///
/// Override with `#[firefly(aggregate_type = "...", field = "...")]`.
#[proc_macro_derive(AggregateRoot, attributes(firefly))]
pub fn derive_aggregate_root(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(eventsourcing::derive_aggregate_root(input))
}

// ===========================================================================
// Event-driven messaging
// ===========================================================================

/// Marks an `async fn(Event) -> FireflyResult<()>` as an event listener and
/// generates a `subscribe_<fn>(broker)` async helper that subscribes it to a
/// topic on a `firefly_eda::Broker`.
///
/// ```ignore
/// #[event_listener("orders.created")]
/// async fn on_order_created(ev: Event) -> FireflyResult<()> { Ok(()) }
/// // generated: async fn subscribe_on_order_created(broker: &dyn Broker) -> EdaResult<()>
/// ```
///
/// Accepts a positional topic or `#[event_listener(topic = "...", group = "...", crate = "...")]`.
#[proc_macro_attribute]
pub fn event_listener(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(eda::event_listener_attr(args.into(), item))
}
