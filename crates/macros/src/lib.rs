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
//! | [`macro@async_method`] | an `async fn(self: Arc<Self>, …) -> R` | a non-async `fn … -> firefly_scheduling::TaskHandle<R>` that spawns the body |
//! | [`macro@rest_controller`] | an `impl` block (`#[get]`/`#[post]`/… methods) | a `routes(state) -> axum::Router` |
//! | [`macro@DomainEvent`] / [`macro@AggregateRoot`] (derive) | a struct | event-type/aggregate ergonomics |
//! | [`macro@event_listener`] | an `async fn(Event) -> FireflyResult<()>` | a `subscribe_<fn>(broker)` helper (EDA broker consumer) |
//! | [`macro@application_event_listener`] | a free `async fn(&E)` | an in-process `@EventListener` (discovered via `inventory`, fired by `publish_event`) |
//! | [`macro@transactional_event_listener`] | a free `async fn(&E)` | a `@TransactionalEventListener` bound to a commit phase (`after_commit` by default) |
//! | [`macro@saga`] / [`macro@workflow`] / [`macro@tcc`] | an `impl` block of `async fn` steps | a `saga()`/`workflow()`/`tcc()` builder + `run` over the step graph (declarative orchestration) |
//! | [`macro@cacheable`] / [`macro@cache_put`] / [`macro@cache_evict`] | an `async fn(...) -> Result<V, E>` | a cache-aware body around the registered adapter |
//! | [`macro@Validate`] (derive) | a struct with `#[validate(...)]` fields | `impl firefly_validators::bean::Validate` (JSR-380 constraints) |
//! | [`macro@aspect`] | an `impl` block (`#[before]`/`#[around]`/… markers) | `impl firefly_aop::Aspect` + an `inventory` registration (declarative AOP) |
//!
//! See each macro's own documentation for the argument surface and an example.
//! These are normally reached through the `firefly` facade
//! (`use firefly::prelude::*;`), which re-exports every macro at its root.

#![forbid(unsafe_code)]

mod aspect;
mod async_exec;
mod bean;
mod builder;
mod cache;
mod common;
mod config_properties;
mod container;
mod cqrs;
mod eda;
mod event_listener;
mod eventsourcing;
mod mapper;
mod method_security;
mod orchestration;
mod repository_query;
mod scheduling;
mod schema;
mod transactional;
mod validate;
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

/// Registers a free `async fn` as an in-process application event listener —
/// Spring's `@EventListener`.
///
/// The handler takes the event by shared reference and runs synchronously
/// (awaited) when a matching event is published with
/// [`publish_event`](firefly_transactional::publish_event). For a listener bound
/// to a transaction phase, see [`transactional_event_listener`]; to subscribe to
/// an EDA message-broker topic instead, see the separate [`macro@event_listener`]
/// (the `@KafkaListener`-style broker subscription).
///
/// ```ignore
/// #[firefly::application_event_listener]
/// async fn on_order_placed(event: &OrderPlaced) {
///     audit_log(event).await;
/// }
/// ```
#[proc_macro_attribute]
pub fn application_event_listener(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(event_listener::application_event_listener_attr(
        args.into(),
        item,
    ))
}

/// Registers a free `async fn` as a transaction-bound event listener — Spring's
/// `@TransactionalEventListener`.
///
/// The handler runs at a [`TransactionPhase`](firefly_transactional::TransactionPhase)
/// of the surrounding transaction — `after_commit` by default, or
/// `before_commit` / `after_rollback` / `after_completion` via `phase = "..."`.
/// An event published with no active transaction falls back to running
/// immediately (as if already committed).
///
/// ```ignore
/// #[firefly::transactional_event_listener]                       // after_commit
/// async fn publish_integration_event(event: &WalletOpened) {
///     bus.send(event).await;   // only after the opening transaction commits
/// }
/// ```
#[proc_macro_attribute]
pub fn transactional_event_listener(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(event_listener::transactional_event_listener_attr(
        args.into(),
        item,
    ))
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

/// Registers a type's **OpenAPI component schema** so the generated document
/// (and Swagger-UI's *Schemas* panel) describes it — the Rust analog of
/// springdoc's `@Schema` model reflection, computed at compile time since Rust
/// has no runtime reflection.
///
/// Derive it on the request/response DTOs your controllers consume and return,
/// then reference them from a route with `#[post(request = OpenWallet,
/// response = WalletView)]`. Each field's Rust type maps to a JSON Schema
/// fragment (`String` → `string`, `Option<T>` → non-required `T`, `Vec<T>` →
/// `array`, `Uuid`/`DateTime` → a typed `format`, a nested DTO → a `$ref` to
/// its own component schema, which should also `#[derive(Schema)]`).
///
/// ```ignore
/// #[derive(serde::Serialize, Schema)]
/// struct WalletView { id: String, balance: u64, frozen: bool, note: Option<String> }
/// ```
#[proc_macro_derive(Schema, attributes(firefly, schema))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(schema::derive_schema(input))
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

/// Runs a method asynchronously, off the caller's task — Spring's `@Async`.
///
/// Rewrites an `async fn name(self: Arc<Self>, args…) -> R` into a **non-async**
/// `fn name(self: Arc<Self>, args…) -> firefly_scheduling::TaskHandle<R>`: the
/// call returns immediately and the original body runs on a
/// [`TaskExecutor`](firefly_scheduling::TaskExecutor)-spawned tokio task. The
/// caller `.await`s (or
/// [`.join()`](firefly_scheduling::TaskHandle::join)s) the returned handle for
/// the result.
///
/// The receiver **must** be `self: Arc<Self>` — the spawned future has to be
/// `'static`, which a `&self`/`self`-by-value receiver cannot provide — so any
/// other receiver is a compile error pointing at the fix.
///
/// By default the work is spawned on the process-global executor
/// ([`firefly_scheduling::task_executor`], an unbounded default when none is
/// registered); pass `executor = "expr"` to spawn on a specific one.
///
/// ```ignore
/// use std::sync::Arc;
///
/// impl Reports {
///     #[firefly::async_method]
///     async fn rebuild(self: Arc<Self>, id: u64) -> u64 {
///         self.heavy_recompute(id).await
///     }
/// }
/// // generated: fn rebuild(self: Arc<Self>, id: u64) -> TaskHandle<u64>
/// let handle = Arc::new(reports).rebuild(7);   // returns at once
/// let total = handle.await.unwrap();           // joins the spawned task
/// ```
///
/// Options: `executor` (a `TaskExecutor` expression to spawn on) and `crate`
/// (facade override).
#[proc_macro_attribute]
pub fn async_method(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(async_exec::async_method_impl(args.into(), item))
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
/// #[rest_controller(path = "/api/v1/orders", tag = "Orders")]
/// impl OrderApi {
///     #[get("/:id", summary = "Fetch an order")]
///     async fn get_order(State(api): State<OrderApi>, Path(id): Path<String>)
///         -> WebResult<Json<OrderView>> { /* … */ }
/// }
/// // generated: fn OrderApi::routes(state: OrderApi) -> axum::Router
/// ```
///
/// Accepts `#[rest_controller(path = "...", state = "MyState", tag = "...",
/// crate = "...")]`. The `state` type defaults to the controller (`Self`); `tag`
/// is the OpenAPI group tag (else derived from the type name).
///
/// ## OpenAPI metadata
///
/// Each verb attribute also feeds the auto-generated OpenAPI document. The
/// operation's **request and response models are inferred** from the handler
/// signature — the `Json<T>` parameter is the request body, the `Json<T>` in the
/// `WebResult<…>` / tuple return is the response — so you rarely name them. Add
/// per-operation metadata inline:
/// `#[get("/x", summary = "…", description = "…", tags = ["A", "B"],
/// status = 200, deprecated, request = T, response = T)]`, where `request` /
/// `response` override the inference (naming a `#[derive(Schema)]` type) and the
/// rest are optional. A `$ref` is only emitted for a registered `Schema`, so an
/// unannotated body never dangles.
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

// ===========================================================================
// Orchestration — declarative sagas, workflows, and TCC
// ===========================================================================

/// Turns an `impl` block into a declarative **saga** (Java/pyfly `@Saga`),
/// generating `MyType::saga(self: Arc<Self>) -> firefly::orchestration::Saga`
/// and a convenience `MyType::run(self: Arc<Self>, input) -> Result<Outcome, SagaFailure>`.
///
/// Each step is an `async fn(&self, ...) -> Result<T, E>` marked
/// `#[saga_step(...)]`; its parameters are injected from the saga context with
/// `#[input]` / `#[input("field")]`, `#[from_step("step-id")]`,
/// `#[variable("key")]`, and `#[ctx]`. A step's `Ok(T)` is serialized and made
/// available to later steps via `#[from_step]`; an `Err(E)` (where
/// `E: std::error::Error + Send + Sync`) triggers compensation in reverse order.
///
/// ```ignore
/// #[firefly::saga(name = "money-transfer", policy = "stop_on_error")]
/// impl TransferSaga {
///     #[saga_step(id = "reserve", compensate = "refund")]
///     async fn reserve(&self, #[input] req: TransferReq) -> Result<Reserved, MyErr> { /* … */ }
///     async fn refund(&self, #[from_step("reserve")] r: Reserved) -> Result<(), MyErr> { /* … */ }
///     #[saga_step(id = "credit", depends_on = ["reserve"], retry = 3, backoff_ms = 100)]
///     async fn credit(&self, #[from_step("reserve")] r: Reserved) -> Result<(), MyErr> { /* … */ }
/// }
/// // generated: TransferSaga::saga(self: Arc<Self>) -> Saga, and ::run(self, input)
/// ```
///
/// `#[saga_step]` options: `id` (required), `depends_on = ["…"]`,
/// `compensate = "method"`, `retry`, `backoff_ms`, `timeout_ms`, `jitter`.
/// `#[saga(...)]` options: `name`, `policy` (best_effort | stop_on_error |
/// retry_with_backoff | circuit_breaker | best_effort_parallel | grouped_parallel),
/// and `crate`.
#[proc_macro_attribute]
pub fn saga(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    emit(orchestration::saga_impl(args.into(), item))
}

/// Turns an `impl` block into a declarative **workflow** — a DAG of nodes with
/// parallel layers, skip conditions, fire-and-forget, and compensation —
/// generating `MyType::workflow(self: Arc<Self>) -> firefly::orchestration::Workflow`
/// and `MyType::run(self: Arc<Self>, input) -> Result<(), WorkflowError>`.
///
/// Each node is an `async fn(&self, ...) -> Result<T, E>` marked
/// `#[workflow_step(...)]`; parameters are injected exactly as for `#[saga]`
/// (`#[input]` / `#[from_step]` / `#[variable]` / `#[ctx]`).
///
/// ```ignore
/// #[firefly::workflow(name = "compliance")]
/// impl Compliance {
///     #[workflow_step(id = "balance-check")]
///     async fn balance(&self, #[input] req: Req) -> Result<bool, MyErr> { /* … */ }
///     #[workflow_step(id = "fraud-scan")]
///     async fn fraud(&self, #[input] req: Req) -> Result<bool, MyErr> { /* … */ }
///     #[workflow_step(id = "approve", depends_on = ["balance-check", "fraud-scan"])]
///     async fn approve(&self, #[from_step("balance-check")] ok: bool) -> Result<(), MyErr> { /* … */ }
/// }
/// ```
///
/// `#[workflow_step]` options: `id` (required), `depends_on = ["…"]`,
/// `compensate = "method"`, `when = "expr"` (skip condition), and
/// `fire_and_forget`. `#[workflow(...)]` options: `name`, `crate`.
#[proc_macro_attribute]
pub fn workflow(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    emit(orchestration::workflow_impl(args.into(), item))
}

/// Turns an `impl` block into a declarative **TCC** coordinator
/// (Try / Confirm / Cancel), generating
/// `MyType::tcc(self: Arc<Self>) -> firefly::orchestration::Tcc` and
/// `MyType::run(self: Arc<Self>, input) -> Result<(), TccError>`.
///
/// Each *try* phase is an `async fn(&self, ...) -> Result<T, E>` marked
/// `#[participant(name = "...", confirm = "...", cancel = "...")]`; the confirm
/// and cancel methods are plain `async fn(&self, ...) -> Result<_, E>` referenced
/// by name. The try result is published under the participant name, so confirm /
/// cancel read it via `#[from_step("<name>")]`.
///
/// ```ignore
/// #[firefly::tcc(name = "transfer-2pc")]
/// impl Transfer2pc {
///     #[participant(name = "source", confirm = "capture_source", cancel = "release_source")]
///     async fn hold_source(&self, #[input] req: Req) -> Result<Hold, MyErr> { /* … */ }
///     async fn capture_source(&self, #[from_step("source")] h: Hold) -> Result<(), MyErr> { /* … */ }
///     async fn release_source(&self, #[from_step("source")] h: Hold) -> Result<(), MyErr> { /* … */ }
/// }
/// ```
///
/// `#[participant]` options: `name` + `confirm` (required), `cancel`, `retry`,
/// `backoff_ms`, `timeout_ms`. `#[tcc(...)]` options: `name`, `crate`.
#[proc_macro_attribute]
pub fn tcc(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    emit(orchestration::tcc_impl(args.into(), item))
}

// ===========================================================================
// Aspect-oriented advice — #[aspect]
// ===========================================================================

/// Turns an `impl` block into a declarative **aspect** — Spring's `@Aspect`
/// (pyfly's `@aspect`).
///
/// The block's methods carry advice markers — `#[before]`, `#[after]`,
/// `#[after_returning]`, `#[after_throwing]`, `#[around]` — naming which
/// [`Aspect`](firefly_aop::Aspect) hook each implements (at most one method per
/// marker). The macro keeps the marked methods callable (it strips only the
/// markers) and generates a `#[async_trait] impl firefly_aop::Aspect for Self`
/// whose hooks delegate to them; only the present hooks are emitted, so the
/// trait's no-op/pass-through defaults cover the rest. It also emits an
/// `inventory` thunk that registers the aspect (built via `Default`) against the
/// pointcut, so it is discovered across the crate graph and woven by
/// [`firefly_aop::advised`] without manual wiring.
///
/// The aspect type must implement [`Default`] — Spring aspects are singletons,
/// and the auto-registered aspect is a single instance constructed via
/// `Default`. The marked method signatures must match the hook shapes
/// (`before`/`after`/`after_returning`/`after_throwing`: `async fn(&self, &JoinPoint)`;
/// `around`: `fn<'a>(&'a self, &'a JoinPoint, Proceed<'a>) -> AdviceFuture<'a>`);
/// a mismatch is a compile error at the generated delegation.
///
/// ```ignore
/// use firefly::prelude::*;
///
/// #[derive(Default)]
/// struct AuditAspect;
///
/// #[firefly::aspect(pointcut = "service.*.*", order = 0)]
/// impl AuditAspect {
///     #[before]
///     async fn log_call(&self, jp: &JoinPoint) {
///         tracing::info!(target = %jp.qualified_name(), "entering");
///     }
///     #[around]
///     fn time_it<'a>(&'a self, _jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
///         Box::pin(async move { proceed.proceed().await })
///     }
/// }
/// // Woven explicitly at the call site:
/// // advised("service.OrderService", "create", args, || async { ok(out) }).await
/// ```
///
/// Options: `pointcut` (required glob), `order` (optional `i32`, default `0`),
/// and `crate` (facade override).
#[proc_macro_attribute]
pub fn aspect(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    emit(aspect::aspect_impl(args.into(), item))
}

// ===========================================================================
// Declarative caching — #[cacheable] / #[cache_put] / #[cache_evict]
// ===========================================================================

/// Read-through caches an `async fn`'s result — Spring's `@Cacheable`
/// (pyfly's `@cacheable`).
///
/// When a process-global cache adapter has been registered through
/// [`firefly_cache::register_cache`], the method first looks the key up in the
/// cache: on a hit it returns the cached value without running the body; on a
/// miss it runs the body exactly once and stores the resulting `Ok(V)` for the
/// configured `ttl` before returning it. A cache-write failure never masks the
/// freshly computed value. When no adapter is registered the method runs its
/// original body unchanged, so caching is a deploy-time concern, not a code
/// change. The function must be `async` and return `Result<V, E>` where
/// `V: serde::Serialize + serde::de::DeserializeOwned`.
///
/// ## Options
/// - `key` (required): a Rust expression yielding a `ToString` value — usually a
///   `format!(...)` over the method's parameters. Evaluated before the body, so
///   a key that borrows a parameter is valid.
/// - `ttl`: a duration literal (`"60s"`, `"500ms"`, `"5m"`, `"1h"`, or a bare
///   integer of seconds); omit for no expiry.
/// - `crate`: facade override for a renamed/shimmed `firefly`.
///
/// ```ignore
/// #[firefly::cacheable(key = "format!(\"order:{}\", id)", ttl = "60s")]
/// async fn load_order(&self, id: u64) -> Result<Order, MyError> {
///     self.repo.fetch(id).await   // runs only on a cache miss
/// }
/// // generated: on hit, returns the cached Order; on miss, runs the body and
/// // stores Order under "order:<id>" for 60s.
/// ```
#[proc_macro_attribute]
pub fn cacheable(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(cache::cacheable_attr(args.into(), item))
}

/// Always runs an `async fn` and writes its result to the cache — Spring's
/// `@CachePut` (pyfly's `@cache_put`).
///
/// Unlike [`macro@cacheable`], no read happens first: the body always executes,
/// and on `Ok(V)` the value is written through under the key (overwriting any
/// existing entry), keeping the cache warm after a mutation. A cache-write
/// failure never masks the returned value. When no adapter is registered the
/// method is a plain call. The function must be `async` and return
/// `Result<V, E>` where `V: serde::Serialize + serde::de::DeserializeOwned`.
///
/// ## Options
/// - `key` (required): a Rust expression yielding a `ToString` value.
/// - `ttl`: a duration literal; omit for no expiry.
/// - `crate`: facade override.
///
/// ```ignore
/// #[firefly::cache_put(key = "format!(\"order:{}\", order.id)", ttl = "60s")]
/// async fn save_order(&self, order: Order) -> Result<Order, MyError> {
///     self.repo.upsert(order).await   // always runs, then refreshes the cache
/// }
/// // generated: runs the body, then stores the returned Order under
/// // "order:<id>" for 60s.
/// ```
#[proc_macro_attribute]
pub fn cache_put(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(cache::cache_put_attr(args.into(), item))
}

/// Evicts a cache entry after an `async fn` succeeds — Spring's `@CacheEvict`
/// (pyfly's `@cache_evict`).
///
/// The body runs first; on `Ok` the keyed entry is removed (or, with
/// `all_entries`, every entry whose key starts with `key` — prefix eviction via
/// `delete_prefix`), so a mutation can invalidate the values a sibling
/// [`macro@cacheable`] would otherwise serve stale. When no adapter is
/// registered the method is a plain call. The function must be `async` and
/// return `Result<V, E>`.
///
/// ## Options
/// - `key` (required): a Rust expression yielding a `ToString` value — the exact
///   key to delete, or the prefix to delete under when `all_entries` is set.
/// - `all_entries`: evict every entry under the `key` prefix instead of a single
///   exact key.
/// - `crate`: facade override.
///
/// ```ignore
/// #[firefly::cache_evict(key = "format!(\"order:{}\", id)")]
/// async fn delete_order(&self, id: u64) -> Result<(), MyError> {
///     self.repo.remove(id).await   // on success, "order:<id>" is evicted
/// }
///
/// #[firefly::cache_evict(key = "\"order:\"", all_entries)]
/// async fn purge_orders(&self) -> Result<(), MyError> { Ok(()) }
/// // generated: runs the body, then evicts the key (or the "order:" prefix).
/// ```
#[proc_macro_attribute]
pub fn cache_evict(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    emit(cache::cache_evict_attr(args.into(), item))
}

// ===========================================================================
// Declarative bean validation — #[derive(Validate)]
// ===========================================================================

/// Derives `firefly_validators::bean::Validate` from per-field
/// `#[validate(...)]` constraints — the JSR-380 (`jakarta.validation`) /
/// Spring `@Valid` analog.
///
/// The generated `validate(&self) -> Result<(), ValidationErrors>` runs
/// *every* constraint and gathers all failures into one `ValidationErrors`
/// (each a `{field, code, message}`), rather than short-circuiting on the
/// first — so a caller (or the `firefly_web::Valid<T>` extractor, which
/// renders the set as a 422 `application/problem+json`) sees the whole list.
///
/// ## Constraints
/// - `email` — RFC 5322 address (reuses [`firefly_validators::validate_email`]).
/// - `url` — `http`/`https` URL with a host.
/// - `not_empty` — non-empty after trimming whitespace.
/// - `length(min = .., max = ..)` — UTF-8 character-count bounds (either bound
///   optional). Applies to any field whose value is `AsRef<str>`.
/// - `range(min = .., max = ..)` — numeric bounds compared with the field's own
///   type (either bound optional).
/// - `pattern = "regex"` — the whole value must match the anchored regex.
/// - `custom = "path::to::fn"` — a user predicate
///   `fn(&FieldTy) -> Result<(), String>`; the returned `String` is the message.
///
/// Several constraints may sit on one field, comma-separated. An unknown
/// constraint is a compile error. The string constraints (`email`, `url`,
/// `not_empty`, `length`, `pattern`) require the field to be `AsRef<str>`
/// (`String`, `&str`, …); `range` requires the field to be `PartialOrd` with
/// the bound literals.
///
/// ```ignore
/// use firefly::prelude::*;
/// use serde::Deserialize;
///
/// #[derive(Deserialize, Validate)]
/// struct CreateUser {
///     #[validate(not_empty, length(max = 80))]
///     name: String,
///     #[validate(email)]
///     email: String,
///     #[validate(range(min = 18, max = 120))]
///     age: u32,
///     #[validate(pattern = "[A-Z]{2}[0-9]+")]
///     code: String,
/// }
/// ```
///
/// Accepts a container-level `#[validate(crate = "...")]` to override the
/// facade segment for a renamed/shimmed `firefly`.
#[proc_macro_derive(Validate, attributes(validate))]
pub fn derive_validate(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    emit(validate::derive_validate(input))
}
