# `firefly-container`

> **Tier:** Platform · **Status:** Full

## Overview

`firefly-container` is an **opt-in, TypeId-keyed dependency-injection
container**. It exposes a *service-locator* surface alongside a
*component-scan* surface — stereotype discovery via
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

| Symbol | Purpose |
|--------|---------|
| `Container::new()` | Empty container |
| `register::<T: Default>()` | Default-constructed singleton |
| `register_factory::<T>(scope, f)` | Register a factory-built bean |
| `register_factory_named::<T>(scope, name, f)` | …with an explicit name |
| `register_factory_scoped::<T>(scope_name, name, f)` | …under a custom scope |
| `register_factory_with::<T>(scope, name, primary, order, f)` | …recording primary/order |
| `register_instance::<T>(obj)` / `_named` | Install a pre-built singleton |
| `bind::<I: ?Sized, T>(coerce)` | Bind an interface to an impl |
| `resolve::<T: ?Sized>()` | Resolve one bean (or trait object) |
| `resolve_named::<T>(name)` | Resolve by name |
| `resolve_all::<T: ?Sized>()` | Resolve every match, ordered |
| `provider::<T>()` | Deferred `Provider<T>` handle |
| `register_scope(name, handler)` / `unregister_scope` | Custom-scope SPI |
| `register_request_scope` / `register_session_scope` | Back the built-in REQUEST/SESSION scopes |
| `contains` / `contains_type` / `registered_types` | Introspection |
| `reset_instance::<T>()` | Evict a cached singleton (refresh hook) |
| `bean_metrics::<T>()` | Per-bean `BeanMetrics` |
| `fuzzy_suggestions(name)` | Hand-rolled similar-name matches |
| `Scope`, `ScopeSpec`, `ScopeHandler`, `RefreshScope` | Lifecycle scopes + SPI |
| `ContainerError::{NoSuchBean, NoUniqueBean, CircularDependency}` | Error taxonomy |
| `HIGHEST_PRECEDENCE`, `LOWEST_PRECEDENCE` | Ordering constants |

### Component scan + conditions + lifecycle (new)

| Symbol | Purpose |
|--------|---------|
| `Container::shared()` / `install_shared_handle` | Arc-wrapped container with a self-handle (needed for `Provider<T>` autowiring) |
| `scan()` / `scan_with(ctx)` | Register every `inventory`-submitted stereotype, two-pass conditional gating |
| `ComponentRegistration` | A link-time scan thunk (one per stereotype derive) |
| `discovered()` / `routes()` | Iterate every scan thunk / `#[rest_controller]` route |
| `set_condition_context` / `condition_context` | Install/read the active profiles + config map |
| `ConditionContext` + `Condition` | Profile + `@ConditionalOn*` evaluation inputs |
| `config_properties(prefix)` | Prefix-stripped config map for `#[derive(ConfigProperties)]` |
| `resolve_value::<T>(c, "${k:default}")` | `@Value` config-field injection |
| `provider_for::<T>()` | Build a `Provider<T>` from a borrowed container (autowiring) |
| `set_stereotype` / `set_destroy_hook` | Record stereotype / `#[pre_destroy]` hook |
| `beans()` / `bean_stats()` / `bean_count()` | Bean introspection for the admin `/beans` view |
| `destroy()` | Run `#[pre_destroy]` hooks in reverse order, evict singletons |
| `BeanDescriptor` / `BeanStats` / `RouteDescriptor` | Introspection + route-metadata records |
| `resolve_named_erased(name)` | Type-erased warm of a named singleton (eager init) |

## Design notes

The container leans on Rust's type system instead of runtime reflection:

- **Derive-generated factory closures.** Rust has no runtime reflection, so the
  stereotype derives in `firefly-macros` generate a `firefly_register` factory
  that resolves each field. The field *type* selects the form: `Arc<T>` →
  `resolve()`, `Vec<Arc<T>>` → `resolve_all()`, `Option<Arc<T>>` →
  `resolve().ok()` (`required=false`), `Provider<T>` → `provider()`, and
  `#[firefly(qualifier = "name")]` → `resolve_named()`. You can still hand-write
  a `register_factory` closure for full control.
- **Link-time `inventory` scanning.** Rust cannot walk a package at runtime, so
  each stereotype derive emits an `inventory::submit!`; `scan()` collects them
  across the whole crate graph and registers each survivor,
  two-pass-evaluating conditions/profiles. *Generic* types cannot be
  inventoried — register those with `register_all!`.
- **`@ConditionalOn*` / `@Profile` via `ConditionContext` + two-pass scan.**
  Property/class/profile conditions evaluate in pass 1; `on_bean` /
  `on_missing_bean` / `on_single_candidate` evaluate in pass 2 against the
  populated registry.
- **`@post_construct` / `@pre_destroy` derive hooks.**
  `#[firefly(post_construct = "method")]` runs after construction;
  `#[firefly(pre_destroy = "method")]` registers a teardown hook run by
  `destroy()` in reverse construction order.
- **`@Value` config injection via `resolve_value`.**
  `#[firefly(value = "${key:default}")]` resolves the placeholder against the
  condition-context config map and parses via `FromStr`. The `#{...}` SpEL form
  is out of scope for the typed-Rust idiom.
- **Auto-bound interfaces via `#[firefly(provides = "dyn Port")]`.** A stereotype
  derive can additionally bind the trait object to itself, so `dyn Port`
  resolves to the impl.
- **Trait-object bindings.** `bind::<dyn Trait, Impl>(|a| a)` registers the
  implementation's registration under `TypeId::of::<dyn Trait>()` together with
  a caster that views the stored `Arc<Impl>` as `Arc<dyn Trait>`. `primary` /
  `order` and `resolve_all::<dyn Trait>()` then behave accordingly.
- **REQUEST / SESSION scopes.** Rust drives request lifecycle explicitly, so
  these built-in scopes are backed by a custom `ScopeHandler` installed via
  `register_request_scope` / `register_session_scope`.
- **Circular-dependency detection** uses a thread-local resolution stack; the
  stack is always cleaned up after a failed resolve.
- **Fuzzy suggestions** are computed dependency-free via a normalized
  longest-common-subsequence ratio (cutoff `0.4`, best 5).

### Tests

The integration tests under `tests/` cover the container semantics:
`container_basics`, `named_beans`, `di_errors`, `custom_scope` (incl. request
scope), `ordering` (incl. same-type beans), `public_spi` (incl. display name),
`provider_and_refresh`, and `scan_introspection` (beans introspection,
reverse-order `destroy`, condition context, prefix-stripped config properties,
`provider_for`). The full macro-driven scan + `#[bean]` + conditional + lifecycle
behavior is exercised end-to-end in `firefly-macros/tests/di.rs`.

[`inventory`]: https://docs.rs/inventory
