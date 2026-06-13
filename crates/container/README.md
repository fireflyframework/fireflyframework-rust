# `firefly-container`

> **Tier:** Platform · **Status:** Full (service-locator + component-scan surface) · **pyfly original:** `pyfly.container` + `pyfly.context`

## Overview

`firefly-container` is an **opt-in, TypeId-keyed dependency-injection
container** ported from pyfly's `container` package and the DI half of
`pyfly.context`. It mirrors the *service-locator* surface faithfully and now
also delivers the *component-scan* surface — stereotype discovery via
[`inventory`], conditional/profile gating, bean lifecycle hooks, `@Value`
config injection, and bean introspection — paired with the
`#[derive(Component)]` / `Service` / `Repository` / `Configuration` /
`Controller` / `ConfigProperties` derives and the `#[bean]` attribute in
`firefly-macros`.

```rust
use firefly_container::{Container, Scope};
use std::sync::Arc;

struct Greeter;
impl Greeter { fn greet(&self) -> &'static str { "hello" } }
struct UserService { greeter: Arc<Greeter> }

let c = Container::new();
c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
c.register_factory::<UserService, _>(Scope::Singleton, |c| {
    Ok(UserService { greeter: c.resolve::<Greeter>()? })
});
let svc = c.resolve::<UserService>().unwrap();
assert_eq!(svc.greeter.greet(), "hello");
```

The container holds its registries (`by_type`, `by_name`, custom `scopes`)
behind `RwLock`s, so it is `Send + Sync` and shareable as `Arc<Container>`
across threads. Trait-object bindings work because `TypeId::of::<dyn Trait>()`
is a valid registry key.

## Public surface

| Symbol | Purpose | pyfly equivalent |
|--------|---------|------------------|
| `Container::new()` | Empty container | `Container()` |
| `register::<T: Default>()` | Default-constructed singleton | `register(cls)` |
| `register_factory::<T>(scope, f)` | Register a factory-built bean | `register(cls, scope=...)` |
| `register_factory_named::<T>(scope, name, f)` | …with an explicit name | `register(cls, name=...)` |
| `register_factory_scoped::<T>(scope_name, name, f)` | …under a custom scope | `register(cls, scope="x")` |
| `register_factory_with::<T>(scope, name, primary, order, f)` | …recording primary/order | `@primary` / `@order` + `@bean` |
| `register_instance::<T>(obj)` / `_named` | Install a pre-built singleton | `register_instance` |
| `bind::<I: ?Sized, T>(coerce)` | Bind an interface to an impl | `bind(interface, impl)` |
| `resolve::<T: ?Sized>()` | Resolve one bean (or trait object) | `resolve` |
| `resolve_named::<T>(name)` | Resolve by name | `resolve_by_name` |
| `resolve_all::<T: ?Sized>()` | Resolve every match, ordered | `resolve_all` / `list[T]` |
| `provider::<T>()` | Deferred `Provider<T>` handle | `Provider[T]` |
| `register_scope(name, handler)` / `unregister_scope` | Custom-scope SPI | `register_scope` |
| `register_request_scope` / `register_session_scope` | Back the built-in REQUEST/SESSION scopes | (driven by `RequestContext`) |
| `contains` / `contains_type` / `registered_types` | Introspection | same |
| `reset_instance::<T>()` | Evict a cached singleton (refresh hook) | `reset_instance` |
| `bean_metrics::<T>()` | Per-bean `BeanMetrics` | `get_bean_metrics` |
| `fuzzy_suggestions(name)` | Hand-rolled similar-name matches | `difflib.get_close_matches` |
| `Scope`, `ScopeSpec`, `ScopeHandler`, `RefreshScope` | Lifecycle scopes + SPI | same |
| `ContainerError::{NoSuchBean, NoUniqueBean, CircularDependency}` | Error taxonomy | `NoSuchBeanError` / `NoUniqueBeanError` / `BeanCurrentlyInCreationError` |
| `HIGHEST_PRECEDENCE`, `LOWEST_PRECEDENCE` | Ordering constants | same |

### Component scan + conditions + lifecycle (new)

| Symbol | Purpose | pyfly equivalent |
|--------|---------|------------------|
| `Container::shared()` / `install_shared_handle` | Arc-wrapped container with a self-handle (needed for `Provider<T>` autowiring) | — |
| `scan()` / `scan_with(ctx)` | Register every `inventory`-submitted stereotype, two-pass conditional gating | `scan_package` / `scan_module_classes` |
| `ComponentRegistration` | A link-time scan thunk (one per stereotype derive) | `__pyfly_injectable__` classes |
| `discovered()` / `routes()` | Iterate every scan thunk / `#[rest_controller]` route | — / route table |
| `set_condition_context` / `condition_context` | Install/read the active profiles + config map | `Environment` / `Config` |
| `ConditionContext` + `Condition` | Profile + `@ConditionalOn*` evaluation inputs | `context.conditions` / `condition_evaluator` |
| `config_properties(prefix)` | Prefix-stripped config map for `#[derive(ConfigProperties)]` | `@config_properties` binding |
| `resolve_value::<T>(c, "${k:default}")` | `@Value` config-field injection | `core.value.Value` |
| `provider_for::<T>()` | Build a `Provider<T>` from a borrowed container (autowiring) | `Provider[T]` |
| `set_stereotype` / `set_destroy_hook` | Record stereotype / `#[pre_destroy]` hook | `__pyfly_stereotype__` / `@pre_destroy` |
| `beans()` / `bean_stats()` / `bean_count()` | Bean introspection for the admin `/beans` view | `BeansProvider` / `OverviewProvider` |
| `destroy()` | Run `#[pre_destroy]` hooks in reverse order, evict singletons | `_call_pre_destroy` |
| `BeanDescriptor` / `BeanStats` / `RouteDescriptor` | Introspection + route-metadata records | `get_beans` / overview / `RequestMapping` |
| `resolve_named_erased(name)` | Type-erased warm of a named singleton (eager init) | eager singleton pass |

## pyfly parity

This crate is the Rust expression of pyfly's `Container`. Adaptation decisions:

- **Reflective autowiring → derive-generated factory closures.** pyfly parses
  `__init__` type hints to wire dependencies; Rust has no runtime reflection, so
  the stereotype derives in `firefly-macros` generate a `firefly_register`
  factory that resolves each field. The field *type* selects the form, matching
  pyfly: `Arc<T>` → `resolve()`, `Vec<Arc<T>>` → `resolve_all()`, `Option<Arc<T>>`
  → `resolve().ok()` (`required=false`), `Provider<T>` → `provider()`, and
  `#[firefly(qualifier = "name")]` → `resolve_named()`. You can still hand-write
  a `register_factory` closure for full control.
- **Package scanning → link-time `inventory`.** Rust cannot walk a package at
  runtime, so each stereotype derive emits an `inventory::submit!`; `scan()`
  collects them across the whole crate graph (the `scan_package` analog) and
  registers each survivor, two-pass-evaluating conditions/profiles. *Generic*
  types cannot be inventoried — register those with `register_all!`.
- **`@ConditionalOn*` / `@Profile` → `ConditionContext` + two-pass scan.**
  Property/class/profile conditions evaluate in pass 1; `on_bean` /
  `on_missing_bean` / `on_single_candidate` evaluate in pass 2 against the
  populated registry, exactly like pyfly's `ConditionEvaluator`.
- **`@post_construct` / `@pre_destroy` → derive hooks.**
  `#[firefly(post_construct = "method")]` runs after construction;
  `#[firefly(pre_destroy = "method")]` registers a teardown hook run by
  `destroy()` in reverse construction order.
- **`@Value` → `resolve_value`.** `#[firefly(value = "${key:default}")]` resolves
  the placeholder against the condition-context config map and parses via
  `FromStr`. The `#{...}` SpEL form is out of scope for the typed-Rust idiom.
- **`_auto_bind_interfaces` → `#[firefly(provides = "dyn Port")]`.** A stereotype
  derive can additionally bind the trait object to itself, so `dyn Port`
  resolves to the impl.
- **Trait-object bindings.** `bind::<dyn Trait, Impl>(|a| a)` registers the
  implementation's registration under `TypeId::of::<dyn Trait>()` together with
  a caster that views the stored `Arc<Impl>` as `Arc<dyn Trait>`. `primary` /
  `order` and `resolve_all::<dyn Trait>()` then behave as in pyfly.
- **REQUEST / SESSION scopes.** pyfly resolves these through a context-local
  `RequestContext`; Rust drives request lifecycle explicitly, so these built-in
  scopes are backed by a custom `ScopeHandler` installed via
  `register_request_scope` / `register_session_scope`.
- **Circular-dependency detection** uses a thread-local resolution stack,
  mirroring pyfly's `Container._resolving`; the stack is always cleaned up after
  a failed resolve.
- **Fuzzy suggestions** are computed dependency-free via a normalized
  longest-common-subsequence ratio (cutoff `0.4`, best 5), standing in for
  `difflib.get_close_matches`.

### Ported tests

The integration tests under `tests/` port pyfly's `tests/container/` semantics:
`container_basics`, `named_beans`, `di_errors`, `custom_scope` (incl. request
scope), `ordering` (incl. same-type beans), `public_spi` (incl. display name),
`provider_and_refresh`, and `scan_introspection` (beans introspection,
reverse-order `destroy`, condition context, prefix-stripped config properties,
`provider_for`). The full macro-driven scan + `#[bean]` + conditional + lifecycle
behavior is exercised end-to-end in `firefly-macros/tests/di.rs`.

[`inventory`]: https://docs.rs/inventory
