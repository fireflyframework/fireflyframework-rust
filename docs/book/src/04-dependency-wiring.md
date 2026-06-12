# Dependency Wiring

Firefly's default style is **explicit construction**: you build your components
with plain constructors and pass them where they are needed, exactly as you
would in idiomatic Rust. For teams that want a Spring-style service locator,
`firefly-container` provides an opt-in DI container. This chapter covers both,
and when to reach for each.

## Explicit construction (the default)

Most services never need a container. You build the infrastructure with
`Core::new`, build your own services with constructors, share them as `Arc<T>`,
and capture them in your handler closures.

```rust,ignore
use std::sync::Arc;
use firefly_starter_core::{Core, CoreConfig};

// Your own service, built by hand.
struct OrderService {
    bus: Arc<firefly_cqrs::Bus>,
    cache: Arc<dyn firefly_cache::Adapter>,
}

impl OrderService {
    fn new(bus: Arc<firefly_cqrs::Bus>, cache: Arc<dyn firefly_cache::Adapter>) -> Self {
        Self { bus, cache }
    }
}

let core = Core::new(CoreConfig { app_name: "orders".into(), ..Default::default() });

// Reuse the Core's wired components — no duplication, no globals.
let svc = Arc::new(OrderService::new(Arc::clone(&core.bus), Arc::clone(&core.cache)));
```

Because every Firefly port is an object-safe trait, "depend on the interface,
inject the implementation" is just an `Arc<dyn Trait>` field:

```rust,ignore
struct NotificationSink {
    channel: Arc<dyn firefly_notifications::Channel>, // swap at construction
}
```

> **Tip** — Explicit wiring keeps the dependency graph visible in code and
> checked by the compiler. There is no reflection and no startup-time magic; if
> it compiles, it is wired. Prefer this for application code.

## The `Core` as a composition root

`Core` is a wired bundle of the components a typical service needs. You read
them straight off the struct, or via accessors that a downstream admin
dashboard can also consume:

| Field / accessor          | Type                                  |
|---------------------------|----------------------------------------|
| `core.bus`                | `Arc<cqrs::Bus>` (validation pre-installed) |
| `core.cache`              | `Arc<dyn cache::Adapter>` (Memory by default) |
| `core.broker`             | `Arc<dyn eda::Broker>` (InMemory by default) |
| `core.scheduler`          | `Arc<scheduling::Scheduler>`           |
| `core.metrics`            | `Arc<actuator::MetricRegistry>`        |
| `core.health`             | `Arc<actuator::HealthComposite>`       |
| `core.health_composite()` | accessor for the same                  |

You can override any of them in `CoreConfig` — drop in a Redis cache, a Kafka
broker, your own bus — and everything downstream uses your choice:

```rust,ignore
use std::sync::Arc;
use firefly_starter_core::{Core, CoreConfig};

let core = Core::new(CoreConfig {
    app_name: "orders".into(),
    cache: Some(Arc::new(my_redis_adapter)),   // Arc<dyn cache::Adapter>
    broker: Some(Arc::new(my_kafka_broker)),    // Arc<dyn eda::Broker>
    ..CoreConfig::default()
});
```

## The opt-in container — `firefly-container`

When you want a Spring-style **service locator** — register everything once,
resolve by type, support scopes and trait-object bindings — reach for
`firefly-container`. It is `TypeId`-keyed, `Send + Sync`, and shareable as
`Arc<Container>`.

```rust
use firefly_container::{Container, Scope};
use std::sync::Arc;

struct Greeter;
impl Greeter {
    fn greet(&self) -> &'static str { "hello" }
}
struct UserService {
    greeter: Arc<Greeter>,
}

let c = Container::new();
c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
c.register_factory::<UserService, _>(Scope::Singleton, |c| {
    Ok(UserService { greeter: c.resolve::<Greeter>()? })
});

let svc = c.resolve::<UserService>().unwrap();
assert_eq!(svc.greeter.greet(), "hello");
```

A factory closure resolves its own dependencies by calling `resolve` — the Rust
analog of constructor injection. There is no reflective autowiring (Rust has no
runtime reflection), so you wire dependencies explicitly inside the closure.

### Scopes

`Scope` controls instance lifecycle:

| Scope                | Behaviour                                           |
|----------------------|-----------------------------------------------------|
| `Scope::Singleton`   | One instance, cached after first resolve            |
| `Scope::Prototype`   | A fresh instance on every `resolve`                 |
| `Scope::Request`     | One per request (backed by `register_request_scope`) |
| `Scope::Session`     | One per session (backed by `register_session_scope`) |
| custom               | Your own `ScopeHandler` via `register_scope`        |

### Trait-object bindings

Bind an interface to an implementation, then resolve the trait object. This is
how you express "depend on the port, get the adapter":

```rust,ignore
use firefly_container::{Container, Scope};

trait Clock: Send + Sync { fn now(&self) -> u64; }
struct SystemClock;
impl Clock for SystemClock { fn now(&self) -> u64 { 0 } }

let c = Container::new();
c.register_factory::<SystemClock, _>(Scope::Singleton, |_| Ok(SystemClock));
c.bind::<dyn Clock, SystemClock>(|impl_arc| impl_arc); // view Arc<SystemClock> as Arc<dyn Clock>

let clock = c.resolve::<dyn Clock>().unwrap();
```

`primary` / `order` metadata (`register_factory_with`) and
`resolve_all::<dyn Trait>()` let you register many implementations and select or
list them, exactly as Spring's `@Primary` / `@Order` and `List<T>` injection do.

### Errors and introspection

The error taxonomy mirrors Spring's: `ContainerError::NoSuchBean`,
`NoUniqueBean`, `CircularDependency`. Circular dependencies are detected with a
thread-local resolution stack. For diagnostics, `fuzzy_suggestions(name)`
returns similar registered names, `registered_types()` lists everything, and
`bean_metrics::<T>()` exposes per-bean resolution stats.

> **Note** — The container is **opt-in**. None of the Go-parity core or the
> starters require it; it exists for teams that prefer a central registry. For
> most application code, explicit construction (above) is simpler, faster to
> compile, and fully checked by the borrow checker.

## Aspect-oriented advice — `firefly-aop`

For cross-cutting behaviour (timing, auditing, retry) without threading it
through every call, `firefly-aop` ports Spring's aspect model: a `Pointcut`
glob matcher, a `JoinPoint`, an `Aspect` with five hooks
(`before`/`after`/`after_returning`/`after_throwing`/`around`), and an
`intercept` chain executor with `around`/`Proceed`. Weaving is explicit at the
call site (there is no class-path proxying), which keeps the control flow
visible.

With the wiring in place, you are ready to build. The reactive model underpins
everything that follows — read [The Reactive Model](./05-reactive-model.md) next.
