# Dependency Wiring

> By the end of this chapter you will understand Lumen's **composition root** —
> the `build_app()` function that resolves every collaborator and hands the
> wallet controller its state — and you will know both ways Firefly lets you
> wire a service: explicit construction (Lumen's choice, for teachability) and
> the best-in-class DI container with component scanning (`#[derive(Service)]`,
> `#[autowired]`, `firefly::scan`). Lumen keeps an explicit root *and* shows the
> scan alternative, so you can pick the style that fits your team.

Every service has a moment where the pieces come together: the cache, the bus,
the repositories, the services that depend on them. Firefly calls that the
*composition root*, and it gives you two ways to write it. The explicit style
builds collaborators with plain constructors and passes them where they are
needed — idiomatic Rust, checked entirely by the compiler. The container style
is a full dependency-injection engine: declare beans with stereotype
derives, mark dependencies `#[autowired]`, and let `firefly::scan` discover and
wire the whole graph. This chapter covers both, using Lumen as the worked
example, and is honest about when to reach for each.

## Lumen's explicit composition root

Lumen wires its application explicitly, in one function, on purpose: a reader
can see the entire dependency graph in a single screen, and the borrow checker
proves it is correct before the program runs. Here is the shape `build_app`
grows into (later chapters add the bus middleware, the ledger, and the security
chain — the *structure* is what matters now):

```rust,ignore
// src/web.rs
use std::sync::Arc;
use firefly::cqrs::QueryCache;
use firefly::eventsourcing::MemoryEventStore;
use firefly::prelude::*;
use firefly::starter_web::WebStack;

/// The fully-assembled Lumen application: the web-tier stack, the CQRS bus, the
/// ledger application service, and the read model.
pub struct LumenApp {
    pub web: WebStack,
    pub bus: Arc<Bus>,
    pub ledger: Ledger,
    pub read_model: Arc<ReadModel>,
    pub query_cache: QueryCache,
}

/// Assembles a `LumenApp` over **in-memory** infrastructure — the default for
/// tests and a no-infra run.
pub async fn build_app() -> LumenApp {
    let web = WebStack::new(firefly::starter_web::CoreConfig {
        app_name: APP_NAME.into(),
        app_version: VERSION.into(),
        ..Default::default()
    });

    // Reuse the WebStack's already-wired collaborators — no duplication,
    // no globals. `web` Derefs to the Core, so `web.bus` / `web.broker`
    // are the same instances the middleware and actuator see.
    let bus = Arc::clone(&web.bus);
    let store: Arc<dyn firefly::eventsourcing::EventStore> = Arc::new(MemoryEventStore::new());
    let broker = Arc::clone(&web.broker);
    let read_model = Arc::new(ReadModel::default());

    let ledger = Ledger::new(Arc::clone(&store), Arc::clone(&broker));
    let query_cache = QueryCache::new();

    LumenApp { web, bus, ledger, read_model, query_cache }
}
```

Three ideas carry the whole design:

- **The root reuses, it does not re-create.** `web.bus` and `web.broker` are the
  instances `WebStack::new` already wired; `build_app` clones the `Arc`s rather
  than standing up a second bus. There is exactly one of each, and everything
  downstream shares it.
- **Ports are `Arc<dyn Trait>` fields.** `store: Arc<dyn EventStore>` is "depend
  on the interface, inject the implementation" with no machinery. Swap
  `MemoryEventStore` for a Postgres-backed store and *only this line* changes —
  the ledger, the handlers, and the controller never notice.
- **The controller gets its state from the root.** When the wallet routes arrive
  in [Your First HTTP API](./06-first-http-api.md), `LumenApp::router()`
  constructs the `WalletApi` controller from these resolved collaborators and
  hands it to the macro-generated router as axum `State`.

> **Tip** — Explicit wiring keeps the dependency graph visible in code and
> checked by the compiler. There is no reflection and no startup-time magic; if
> it compiles, it is wired. For an application as focused as Lumen, this is the
> clearest choice — and it is why the book uses it.

> **Design note.** This is constructor injection done by hand: the
> `LumenApp { .. }` literal is the place where dependencies are "injected." Lumen
> spells the graph out explicitly rather than inferring it — the trade-off is
> verbosity for transparency, and for many Rust services transparency wins.

## The container as a composition root

`WebStack`/`Core` is itself a wired bundle of the components a typical service
needs. You read them straight off the struct (Lumen does), or pass overrides in
`CoreConfig`:

| Field / accessor          | Type                                  |
|---------------------------|----------------------------------------|
| `web.bus`                 | `Arc<cqrs::Bus>` (validation pre-installed) |
| `web.cache`               | `Arc<dyn cache::Adapter>` (Memory by default) |
| `web.broker`              | `Arc<dyn eda::Broker>` (InMemory by default) |
| `web.scheduler`           | `Arc<scheduling::Scheduler>`           |
| `web.metrics`             | `Arc<actuator::MetricRegistry>`        |
| `web.health`              | `Arc<actuator::HealthComposite>`       |

```rust,ignore
let web = WebStack::new(firefly::starter_web::CoreConfig {
    app_name: "lumen".into(),
    cache: Some(Arc::new(my_redis_adapter)),   // Arc<dyn cache::Adapter>
    broker: Some(Arc::new(my_kafka_broker)),    // Arc<dyn eda::Broker>
    ..Default::default()
});
```

Override any field and everything downstream uses your choice — the same "swap
the adapter, keep the code" move you will make per subsystem throughout the book.

## The best-in-class DI container — `firefly-container`

When you would rather *declare* beans and let the framework discover and wire
them, Firefly ships a full dependency-injection
container with **component scanning**, stereotype derives, constructor-style
`#[autowired]` injection, qualifier/primary/order disambiguation, `Vec` and
`Provider` injection, `#[bean]` factories, lifecycle hooks, and
conditional/profile gating. It is `TypeId`-keyed, `Send + Sync`, and shareable
as `Arc<Container>`. Beans default to singleton lifetime; the container also
supports transient, request, and session scopes, which [Dependency
Injection](./04a-dependency-injection.md) covers in full.

### Stereotypes — declaring your beans

You make a type visible to the container by deriving a **stereotype**. All five
register the type as a managed bean; the difference is the architectural role
each name communicates (and that the web layer uses to find controllers):

| Derive                     | Role                                                   |
|----------------------------|--------------------------------------------------------|
| `#[derive(Service)]`       | Business-logic layer: use-case orchestration.          |
| `#[derive(Component)]`     | Generic managed bean with no specific role.            |
| `#[derive(Repository)]`    | Data-access layer: databases, external storage, ports. |
| `#[derive(Configuration)]` | A factory holder that can carry `#[bean]` methods.     |
| `#[derive(Controller)]`    | HTTP layer (`#[rest_controller]` builds on this).      |

> **Design note.** The five stereotype derives document an architectural role —
> business logic (`Service`), generic managed bean (`Component`), data access
> (`Repository`), factory configuration (`Configuration`), or HTTP layer
> (`Controller`). The role is recorded on each bean, so the admin dashboard's
> `/beans` view can group beans by layer.

### `#[autowired]` — constructor injection without a constructor

Mark a field `#[autowired]` and the container fills it in by type. This is the
Rust spelling of constructor injection — you declare *what* a bean needs; the
container supplies it. Here is how Lumen's ledger-and-read-model pair would look
in container style:

```rust,ignore
use std::sync::Arc;
use firefly::prelude::*;

#[derive(Repository, Default)]
struct WalletReadModel { /* in-memory rows */ }

#[derive(Service)]
struct WalletService {
    #[autowired]
    read_model: Arc<WalletReadModel>,   // resolved by type, recursively
}
```

When the container constructs `WalletService` it first constructs
`WalletReadModel`, then injects it. A dependency that does not exist surfaces as
a clear resolution error at startup — not a panic three frames deep at runtime.

`#[autowired]` injects more than a single `Arc<T>`:

- `#[autowired] widgets: Vec<Arc<Widget>>` injects **every** registered `Widget`,
  ordered by each bean's `order` (collection injection).
- `#[autowired] maybe: Option<Arc<Thing>>` injects `Some` when a `Thing` is
  registered and `None` when it is not — an optional dependency.
- `#[autowired] tickets: Provider<Ticket>` injects a **deferred** handle:
  `tickets.get()` resolves a fresh instance on each call, the way you would
  reach for a transient bean inside a singleton.

### Component scanning — `firefly::scan`

Rust has no runtime package introspection, so discovery is **link-time**: every
stereotype derive emits an `inventory` thunk, and `firefly::scan(&container)`
(equivalently `container.scan()`) collects every submitted thunk across the
whole crate graph and registers them — honoring conditionals and profiles as it
goes. The usual entry point is the `ApplicationContext`, which wraps the
container with the full startup sequence:

```rust,ignore
use firefly::prelude::*;

let ctx = ApplicationContext::builder()
    .profiles(["test"])
    .property("feature.audit", "on")
    .build();
let c = ctx.container();

// Every stereotype-derived bean in the crate graph is discovered and wired.
let svc = c.resolve::<WalletService>().expect("scan registered the service");
```

> **Design note.** `firefly::scan` / `ApplicationContext::builder()` discover
> every stereotype-derived type in the linked crate graph and register it,
> subject to its conditions and the active profiles. Because Rust has no runtime
> reflection, discovery is link-time (via `inventory`): a bean is discoverable
> exactly when its crate is compiled into the binary. The one Rust-specific note:
> generic types can't be inventoried (the monomorphization is chosen at the use
> site), so you register those with the explicit `register_all!` fallback below.

### Interface auto-binding, `primary`, `order`, and qualifiers

Bind a trait object to an implementation right on the derive, and resolve the
trait afterward — "depend on the port, get the adapter":

```rust,ignore
trait Clock: Send + Sync { fn now(&self) -> u64; }

#[derive(Component, Default)]
#[firefly(provides = "dyn Clock", primary)]
struct SystemClock;
impl Clock for SystemClock { fn now(&self) -> u64 { 42 } }

// elsewhere: c.resolve::<dyn Clock>() yields the SystemClock instance.
```

When several beans satisfy the same interface, `#[firefly(... primary)]` picks
the default for `resolve`, and `resolve_all::<dyn Trait>()` returns *all* of
them ordered by `order`. For the rare case
where you need a *specific* named instance rather than any satisfying one, the
container supports qualifier-by-name resolution.

Declaring `provides =` on the derive is the scan-friendly way to bind a trait to
an implementation. When you are wiring a container by hand instead, the
equivalent move is an explicit `Container::bind::<dyn Trait, Concrete>()` call;
both register the same trait-to-adapter mapping. [Dependency
Injection](./04a-dependency-injection.md) covers `bind`, named beans, and the
full disambiguation surface in depth.

### `#[bean]` factories — wiring things you do not own

Not every dependency is a type you can annotate. Third-party clients need
constructor arguments; some beans are clearest as a factory. A
`#[derive(Configuration)]` holder with `#[firefly::bean]` methods produces beans
keyed by their **return type** — the way you wire a third-party client or a
hand-built adapter the container does not construct directly:

```rust,ignore
use firefly::prelude::*;

#[derive(Configuration, Default)]
struct LumenInfraConfig;

#[firefly::bean]
impl LumenInfraConfig {
    // Registered as `dyn EventStore` — swap MemoryEventStore for a Postgres
    // store here and nothing else in Lumen changes.
    #[bean(primary)]
    fn event_store(&self) -> Arc<dyn firefly::eventsourcing::EventStore> {
        Arc::new(firefly::eventsourcing::MemoryEventStore::new())
    }
}
```

`#[bean]` methods may declare parameters; the container resolves each by type
before the method runs. A `#[bean(profile = "prod")]` method registers only when
the `prod` profile is active — the factory-level twin of the conditional gating
below.

### Lifecycle hooks — `#[post_construct]` / `#[pre_destroy]`

Real infrastructure beans need to *act* once wired (open a pool, subscribe to a
topic) and undo it on shutdown. Name the methods on the derive:

```rust,ignore
#[derive(Service, Default)]
#[firefly(post_construct = "started", pre_destroy = "stopped")]
struct ProjectionSubscriber { /* ... */ }

impl ProjectionSubscriber {
    fn started(&mut self) { /* subscribe the read-model projection */ }
    fn stopped(&self)     { /* drain and unsubscribe */ }
}
```

`post_construct` runs after construction and injection complete; `pre_destroy`
runs on `container.destroy()` in **reverse** construction order, so a subscriber
started after the store is torn down before it. This is precisely the lifecycle
Lumen drives by hand in `main` today (subscribe the projection, then run); the
container would manage it for you.

> **Design note.** `#[firefly(post_construct = "...", pre_destroy = "...")]` name
> a method to run after a bean's dependencies are injected and a method to run on
> shutdown, with a "destroy in reverse construction order" guarantee.

### Conditional and profile gating — the same codebase in every environment

Conditions answer "should this bean exist at all, given the environment?" — how
one codebase runs with cheap in-memory adapters in dev and real infrastructure
in prod, without an `if` in your service code:

```rust,ignore
// Registered only when the property is present and not false.
#[derive(Service, Default)]
#[firefly(condition_on_property = "feature.audit=on")]
struct AuditService;

// Registered only under the named profile.
#[derive(Service, Default)]
#[firefly(profile = "prod")]
struct PostgresHealthCheck;
```

`firefly::scan` evaluates these as it collects each thunk, so the resolved
container holds exactly the beans the environment calls for. This is the
mechanism behind the "swap the adapter" callouts throughout the book — and the
reason Lumen can stay in-memory for teaching while a production deployment flips
to real infrastructure through configuration alone.

### `register_all!` — the explicit fallback

When you want an explicit list (for generics that can't be scanned, or simply to
keep wiring local to one test), `register_all!` registers a known set on a
container:

```rust,ignore
let c = Container::new();
firefly::register_all!(&c, [WalletReadModel, WalletService]);
let svc = c.resolve::<WalletService>().expect("service resolves");
```

### Errors and introspection

The error taxonomy is precise: a missing bean, a non-unique bean with no
`primary`, and a detected circular dependency each surface as a distinct,
named error at resolution time. For diagnostics, the container can list its
registered beans and report per-bean resolution stats — the data the admin
dashboard's `/beans` view renders.

## Choosing a style

Both styles are first-class; neither is required by the core or the starters.

- **Explicit construction** (Lumen's choice) keeps the graph visible and
  compiler-checked, compiles faster, and reads top-to-bottom. Prefer it for a
  focused service whose wiring fits on a screen.
- **The container** shines as a service grows many beans, many adapters, and
  many environment-specific variations — when "declare the bean, let scan find
  it" removes more boilerplate than the indirection costs. Reach for it when you
  want declarative, scan-driven wiring or the `/beans` introspection.

You can even mix them: keep an explicit root and resolve a scanned sub-graph
from a `Container` field on `LumenApp`.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| `build_app` understood only as "the thing `main` calls" | named as the **composition root**: it reuses the `WebStack`'s wired collaborators and hands them down |
| ports felt abstract | seen concretely as `Arc<dyn EventStore>` / `Arc<dyn Broker>` fields — one line to swap an adapter |
| only one wiring style implied | both styles in hand: explicit (Lumen's) and the full DI container with scan, stereotypes, `#[autowired]`, `#[bean]`, lifecycle, and conditions |

## Exercises

1. **Trace the single bus.** In `build_app`, print `Arc::strong_count(&bus)`
   before and after constructing `LumenApp`. Confirm there is exactly one `Bus`
   shared by the middleware, the controller, and the handlers — never a second
   one.
2. **Scan a two-bean graph.** Recreate the `WalletReadModel` / `WalletService`
   pair above, build an `ApplicationContext`, and `resolve::<WalletService>()`.
   Then add `#[firefly(condition_on_property = "wallet.enabled=true")]` to the
   service and watch it disappear from the container until you set the property.
3. **Auto-bind a port.** Define a `Clock` trait, give `SystemClock`
   `#[firefly(provides = "dyn Clock", primary)]`, and resolve `dyn Clock`. Add a
   second implementation without `primary` and observe the non-unique-bean error;
   move `primary` to fix it.
4. **Produce a bean from a factory.** Move Lumen's `MemoryEventStore` behind a
   `#[derive(Configuration)]` holder with a `#[bean]` method returning
   `Arc<dyn EventStore>`, and resolve the trait object. Note that swapping in a
   real store is now a one-method change — the same seam the explicit root has.

With the wiring understood, the reactive primitives underpin everything that
follows — read [The Reactive Model](./05-reactive-model.md) next, then give
Lumen its first endpoints in [Your First HTTP API](./06-first-http-api.md).
