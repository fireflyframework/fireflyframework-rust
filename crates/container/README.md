# `firefly-container`

> **Tier:** Platform · **Status:** Full (service-locator surface) · **pyfly original:** `pyfly.container`

## Overview

`firefly-container` is an **opt-in, TypeId-keyed dependency-injection
container** ported from pyfly's `container` package. It mirrors the
*service-locator* half of pyfly's surface faithfully and adapts the
*reflective-autowiring* half (which has no Rust analog) to explicit factory
closures.

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

## pyfly parity

This crate is the Rust expression of pyfly's `Container`. Adaptation decisions:

- **Reflective autowiring → explicit factory closures.** pyfly parses
  `__init__` type hints to wire constructor dependencies; Rust has no runtime
  reflection, so a `register_factory` closure resolves its own dependencies by
  calling `resolve`. `Optional[T]` becomes `resolve().ok()`, `list[T]` becomes
  `resolve_all()`, and `Qualifier("name")` becomes `resolve_named("name")`.
- **`Autowired` field injection → dropped** (constructor-only is the idiom).
- **Package scanning + stereotype decorators → dropped** (no `importlib`);
  registration is explicit.
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
and `provider_and_refresh`.
