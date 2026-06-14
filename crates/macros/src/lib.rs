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
mod builder;
mod common;
mod config_properties;
mod container;
mod cqrs;
mod eda;
mod eventsourcing;
mod mapper;
mod method_security;
mod repository_query;
mod scheduling;
mod transactional;
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

/// Derives a DI registration for an **auto-configuration** holder (Spring Boot
/// `@AutoConfiguration`) — a [`macro@Configuration`] whose `#[bean]` factory
/// methods are contributed *last* during `Container::scan()`. Pair each
/// `#[bean]` with `#[bean(condition_on_missing_bean = "Type")]` so a
/// user-defined bean of the same type always takes precedence: the two-pass
/// scan registers unconditional (user) beans first, then only fills the gaps
/// from auto-configuration. This is how starters contribute sensible defaults
/// without overriding anything the application declares.
///
/// ```ignore
/// #[derive(AutoConfiguration)]
/// struct RedisAutoConfiguration;
///
/// #[firefly::bean]
/// impl RedisAutoConfiguration {
///     #[bean(condition_on_missing_bean = "CacheClient", condition_on_property = "cache.type=redis")]
///     fn cache_client(&self) -> CacheClient { CacheClient::default() }
/// }
/// ```
#[proc_macro_derive(AutoConfiguration, attributes(firefly, autowired))]
pub fn derive_auto_configuration(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(container::derive_component(
        input,
        Stereotype::AutoConfiguration,
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

/// Wraps an `async fn` in a transaction — Spring's `@Transactional`.
///
/// The function runs through the registered
/// [`TransactionManager`](firefly_transactional::TransactionManager): the body
/// is committed if it returns `Ok` and rolled back if it returns `Err`, so an
/// ordinary `repo.save(a).await?; repo.save(b).await?;` is atomic. The error
/// type must implement `From<firefly_transactional::TxError>`.
///
/// ```ignore
/// #[firefly::transactional(propagation = "requires_new", isolation = "serializable")]
/// async fn transfer(&self, from: Id, to: Id, cents: u64) -> Result<(), MyError> {
///     self.accounts.debit(from, cents).await?;
///     self.accounts.credit(to, cents).await?;   // both commit, or neither
///     Ok(())
/// }
/// ```
///
/// Options: `propagation` (required | requires_new | nested | supports |
/// not_supported | mandatory | never), `isolation` (default | read_uncommitted
/// | read_committed | repeatable_read | serializable), `read_only`,
/// `timeout_ms`, and `crate` (facade override).
#[proc_macro_attribute]
pub fn transactional(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(transactional::transactional_impl(args.into(), item))
}

/// Guards a method *before* it runs — Spring Security's `@PreAuthorize`.
///
/// The rule is checked against the ambient
/// [`Authentication`](firefly_security::Authentication) that
/// [`BearerLayer`](firefly_security::BearerLayer) scopes around the request, so
/// it works on a service method that never sees the `Request`. The function
/// must return `Result<T, E>` with `E: From<firefly_security::SecurityError>`;
/// a denial surfaces as `Err` (`Unauthenticated` when no caller is present,
/// `Forbidden` when the caller's authorities don't match).
///
/// ```ignore
/// #[firefly::pre_authorize(role = "ADMIN")]
/// async fn close_books(&self) -> Result<(), MyError> { /* ... */ }
///
/// #[firefly::pre_authorize(any_role = ["ADMIN", "AUDITOR"])]
/// async fn export(&self) -> Result<Report, MyError> { /* ... */ }
/// ```
///
/// Rules: `authenticated` (default when empty), `role = ".."`,
/// `any_role = [".."]`, `authority = ".."`, `any_authority = [".."]`.
#[proc_macro_attribute]
pub fn pre_authorize(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(method_security::pre_authorize_impl(args.into(), item))
}

/// Authorizes a method's *return value* — Spring Security's `@PostAuthorize`.
///
/// The `async fn` body runs first; then the boolean expression is evaluated
/// with `result` (a `&T` to the returned value, the `returnObject`) and `auth`
/// (a `&Authentication`) in scope. A `false` result denies with `Forbidden` and
/// discards the value. The function must return `Result<T, E>` with
/// `E: From<firefly_security::SecurityError>`.
///
/// ```ignore
/// #[firefly::post_authorize(result.owner == auth.principal)]
/// async fn load(&self, id: Id) -> Result<Wallet, MyError> { /* ... */ }
/// ```
#[proc_macro_attribute]
pub fn post_authorize(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(method_security::post_authorize_impl(args.into(), item))
}

/// Derives a fluent builder — Lombok's `@Builder`.
///
/// `T::builder().field(v)…​.build()` returns `Result<T, String>`; required
/// fields error if unset. Field attributes: `#[builder(into)]` (setter takes
/// `impl Into<Ty>`), `#[builder(default)]` (fall back to `Default::default()`),
/// `#[builder(default = "expr")]` (fall back to a custom expression).
///
/// ```ignore
/// #[derive(Builder)]
/// struct OpenAccount { owner: String, #[builder(default)] overdraft: i64 }
/// let cmd = OpenAccount::builder().owner("ada").build().unwrap();
/// ```
#[proc_macro_derive(Builder, attributes(builder))]
pub fn derive_builder(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(builder::derive_builder(input))
}

/// Derives compile-time, type-checked `From<Source>` conversions — MapStruct's
/// `@Mapper`.
///
/// `#[firefly(from = "Source")]` (repeatable) on the struct; per-field
/// `#[firefly(rename = "src_field")]`, `#[firefly(into)]`,
/// `#[firefly(with = "fn")]`, `#[firefly(default)]`,
/// `#[firefly(default_expr = "expr")]`. Zero-cost and fully compiler-verified,
/// unlike the runtime [`firefly_data::Mapper`] fallback.
///
/// ```ignore
/// #[derive(Mapper)]
/// #[firefly(from = "AccountEntity")]
/// struct AccountDto { id: u64, #[firefly(rename = "owner_name")] owner: String }
/// let dto: AccountDto = entity.into();
/// ```
#[proc_macro_derive(Mapper, attributes(firefly))]
pub fn derive_mapper(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(mapper::derive_mapper(input))
}

/// Generates Spring-Data **derived-query** method bodies on an `impl` block —
/// declare a typed `find_by_…` / `count_by_…` / `exists_by_…` / `delete_by_…`
/// method and get a working, compiler-checked implementation over the tested
/// query engine.
///
/// ```ignore
/// #[firefly::repository]
/// impl AccountRepo {
///     async fn find_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }
///     async fn count_by_owner(&self, owner: &str)  -> Result<i64, DataError>          { unimplemented!() }
/// }
/// ```
///
/// The impl-block type exposes the backing repository via `self.repository()`
/// (override with `#[repository(repo = "field_or_method")]`). Return shapes:
/// `Result<Vec<T>|Option<T>|i64|bool|u64, firefly::data::DataError>`. The
/// placeholder body is replaced by the macro.
#[proc_macro_attribute]
pub fn repository(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    emit(repository_query::repository_impl(args.into(), item))
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
