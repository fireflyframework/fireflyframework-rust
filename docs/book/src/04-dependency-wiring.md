# Dependency Wiring

> By the end of this chapter you will understand how Lumen is wired: it
> **declares beans** and lets the framework's dependency-injection container
> discover and connect them. There is no hand-written composition root — every
> collaborator is a bean (`#[derive(Configuration)]` + `#[bean]` factories,
> `#[derive(Controller)]` controllers), every dependency is `#[autowired]` by
> type, and `FireflyApplication` component-scans the whole graph at boot. This
> is the best-in-class DI container that powers the framework, told against
> Lumen's own collaborators.

Every service has a moment where the pieces come together: the cache, the bus,
the event store, the ledger, the controller that depends on them. In a Firefly
service that moment is **not** a hand-written function — it is the framework's
component scan. You *declare* each collaborator as a bean with a stereotype
derive, mark its dependencies `#[autowired]`, and `FireflyApplication` discovers
and wires the whole object graph when it boots. This chapter walks that path
using Lumen's real beans, then surveys the full container surface
[the next chapter](./04a-dependency-injection.md) covers in depth.

## How `FireflyApplication` wires the object graph

When `FireflyApplication::new("lumen").run()` boots (the one-line `main` from
the [Quickstart](./02-quickstart.md)), it does the wiring a composition root
used to do by hand:

1. It builds the web stack and **auto-registers** the framework's infrastructure
   beans — the CQRS `Bus`, the event `Broker`, the cache, the metric registry,
   the scheduler — into the container so your beans can autowire them.
2. It **component-scans** the crate graph: every stereotype-derived type and
   every `#[bean]` factory in Lumen is discovered, condition-checked, and
   registered.
3. It **resolves** the controllers to mount them, which recursively constructs
   their autowired collaborators in dependency order — exactly the graph a
   hand-written root would build, but derived from the bean declarations instead
   of spelled out.

The whole graph lives in `src/web.rs` as declarations next to each type. Two
declarations carry it: a `#[derive(Configuration)]` holder whose `#[bean]`
methods declare the domain beans, and a `#[derive(Controller)]` whose
`#[autowired]` fields name what it needs.

## Lumen's beans — `#[derive(Configuration)]` + `#[bean]`

Not every collaborator is a type you can annotate directly: the event store, the
read model, the query cache, the JWT service, and the ledger are all built by a
factory. Lumen declares them on a `#[derive(Configuration)]` holder — the Spring
`@Configuration` + `@Bean` analog. Each `#[bean]` method is keyed by its return
type, and the container resolves each method's `Arc<Dep>` arguments from the
container before calling it, so a factory can depend on other beans:

```rust,ignore
// src/web.rs
use std::sync::Arc;
use firefly::cqrs::QueryCache;
use firefly::eda::Broker;
use firefly::eventsourcing::{EventStore, MemoryEventStore};
use firefly::prelude::*;
use firefly::security::{BearerLayer, FilterChain, JwtService};

/// Lumen's `@Configuration` holder. Its `#[bean]` factory methods **declare**
/// the app's domain beans. `container.scan()` discovers and registers them —
/// the framework does the registration, so there is no `register_arc` to call.
#[derive(Configuration)]
struct LumenBeans;

#[bean]
impl LumenBeans {
    /// The in-memory event store (`@Bean`).
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }

    /// The read model the projection feeds and `GetWallet` serves (`@Bean`).
    #[bean]
    fn read_model(&self) -> ReadModel {
        ReadModel::default()
    }

    /// The read-side query cache honouring `GetWallet`'s 30s TTL (`@Bean`).
    #[bean]
    fn query_cache(&self) -> QueryCache {
        QueryCache::new()
    }

    /// The HS256 JWT service (`@Bean`).
    #[bean]
    fn jwt_service(&self) -> JwtService {
        JwtService::new(crate::security::DEMO_SIGNING_KEY)
    }

    /// The security filter chain + bearer layer — auto-discovered and applied
    /// by `FireflyApplication`, no `.security(...)` call (Chapter 14).
    #[bean]
    fn security_filter_chain(&self) -> FilterChain {
        crate::security::security_layers().1
    }
    #[bean]
    fn bearer_layer(&self) -> BearerLayer {
        crate::security::security_layers().0
    }

    /// The ledger application service — its parameters are **autowired**: the
    /// container resolves the event store, the framework-provided `Broker` port,
    /// and the read model by type, then hands them to the factory.
    #[bean]
    fn ledger(
        &self,
        store: Arc<MemoryEventStore>,
        broker: Arc<dyn Broker>,
        read_model: Arc<ReadModel>,
    ) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

Three ideas carry the whole design:

- **The framework does the registration.** You never call `register_arc` or
  `Container::bind`: `container.scan()` discovers the `LumenBeans` holder *and*
  each of its `#[bean]` methods (every method submits its own link-time scan
  thunk) and registers the produced values keyed by their return type.
- **Ports are resolved by type.** The `ledger` factory takes `broker: Arc<dyn
  Broker>` — "depend on the interface, inject the implementation" — and the
  container supplies the framework's broker. Swap `MemoryEventStore` for a
  Postgres-backed store by changing *only this holder*; the ledger, the
  handlers, and the controller never notice.
- **A bean can depend on a bean.** `ledger(&self, store, broker, read_model)`
  pulls in three other beans by type. The container builds them first, then
  calls the factory — the same dependency-ordered construction a hand-written
  root would do, derived from the parameter types.

> **Tip** — Declarative wiring keeps each collaborator's dependencies *next to
> the collaborator*, and the container resolves them by type. A missing
> dependency is a clear resolution error at startup — not a panic three frames
> deep at runtime. The startup report logs every bean it registered, so "what is
> wired" is printed line-by-line at boot.

> **Design note.** A `#[derive(Configuration)]` holder with `#[bean]` methods is
> the Spring `@Configuration` + `@Bean` analog: a factory whose methods produce
> beans keyed by return type, resolving their own arguments from the container.
> Lumen declares its whole domain graph this way, and the framework's component
> scan turns the declarations into the wired object graph.

## The container as a composition root

The container *is* Lumen's composition root: `FireflyApplication` scans it, and
the framework's own infrastructure beans — the bus, broker, cache, scheduler,
and metric/health registries — are pre-registered into it before the scan, so
any bean can autowire them. The infrastructure surface available by type:

| Bean (resolve by type)    | Type                                  |
|---------------------------|----------------------------------------|
| `Bus`                     | `Arc<cqrs::Bus>` (validation pre-installed) |
| cache adapter             | `Arc<dyn cache::Adapter>` (Memory by default) |
| broker                    | `Arc<dyn eda::Broker>` (InMemory by default) |
| scheduler                 | `Arc<scheduling::Scheduler>`           |
| metric registry           | `Arc<actuator::MetricRegistry>`        |
| health composite          | `Arc<actuator::HealthComposite>`       |

Tune the underlying `WebStack`/`Core` knobs through
`FireflyApplication::configure(|cfg: &mut CoreConfig| { … })` — but the
collaborators themselves you reach by autowiring them into a bean.

## The DI container under the hood — `firefly-container`

The container the scan drives is a full dependency-injection engine with
**component scanning**, stereotype derives, constructor-style `#[autowired]`
injection, qualifier/primary/order disambiguation, `Vec` and `Provider`
injection, `#[bean]` factories, lifecycle hooks, and conditional/profile gating.
It is `TypeId`-keyed, `Send + Sync`, and shareable as `Arc<Container>`. Beans
default to singleton lifetime; the container also supports transient, request,
and session scopes, which [Dependency
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
container supplies it. This is exactly how Lumen's wallet controller names its
collaborators (the real `WalletApi` from `src/web.rs`):

```rust,ignore
use std::sync::Arc;
use firefly::cqrs::QueryCache;
use firefly::prelude::*;

/// The wallet HTTP surface — a `#[derive(Controller)]` DI bean. Its
/// collaborators are autowired from the container; `#[rest_controller]`
/// auto-mounts it (Chapter 6).
#[derive(Clone, Controller)]
pub struct WalletApi {
    #[autowired]
    pub bus: Arc<Bus>,            // the CQRS bus it dispatches through
    #[autowired]
    pub ledger: Arc<Ledger>,      // the application service the saga + stream use
    #[autowired]
    pub query_cache: Arc<QueryCache>,  // invalidated after a mutation
}
```

When the container constructs `WalletApi` it first resolves the `Bus`, the
`Ledger` (recursively building *its* store, broker, and read model from the
`#[bean]` factories above), and the `QueryCache`, then injects all three. A
dependency that does not exist surfaces as a clear resolution error at startup —
not a panic three frames deep at runtime.

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
goes. **`FireflyApplication` runs this scan for you at boot**; the lower-level
entry point is the `ApplicationContext`, which wraps the container with the full
startup sequence and is handy in a focused test:

```rust,ignore
use firefly::prelude::*;

let ctx = ApplicationContext::builder()
    .profiles(["test"])
    .property("feature.audit", "on")
    .build();
let c = ctx.container();

// Every stereotype-derived bean in the crate graph is discovered and wired.
let api = c.resolve::<WalletApi>().expect("scan registered the controller");
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
constructor arguments; some beans are clearest as a factory. This is the
`LumenBeans` holder you already saw — a `#[derive(Configuration)]` with `#[bean]`
methods that produce beans keyed by their **return type** — the way Lumen wires
its event store, read model, query cache, JWT service, and ledger. A single
factory can swap an implementation in one place:

```rust,ignore
#[bean]
impl LumenBeans {
    // Swap MemoryEventStore for a Postgres store here and nothing else in
    // Lumen changes — the `ledger` factory depends on the EventStore *port*.
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }
}
```

`#[bean]` methods may declare parameters; the container resolves each by type
before the method runs (Lumen's `ledger` factory does exactly that). A
`#[bean(profile = "prod")]` method registers only when the `prod` profile is
active — the factory-level twin of the conditional gating below.

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
started after the store is torn down before it — the container-managed form of
the kind of one-time wiring Lumen's `ledger` `#[bean]` does when it seeds the
event-sourcing projection on construction.

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
firefly::register_all!(&c, [ReadModel, Ledger, WalletApi]);
let api = c.resolve::<WalletApi>().expect("controller resolves");
```

### Errors and introspection

The error taxonomy is precise: a missing bean, a non-unique bean with no
`primary`, and a detected circular dependency each surface as a distinct,
named error at resolution time. For diagnostics, the container can list its
registered beans and report per-bean resolution stats — the data the admin
dashboard's `/beans` view renders.

## Scan-driven wiring vs. an explicit list

The container is the path Lumen — and the framework — take: declare beans, let
`FireflyApplication`'s component scan discover and wire them, and read the result
off the `/beans` view. There is one explicit escape hatch, `register_all!`, for
the cases the scan can't reach:

- **Component scanning** is the default. `#[derive(...)]` a stereotype (or add a
  `#[bean]` factory) and the bean is discovered link-time, condition-checked, and
  wired — no list to maintain. This is how every bean in `samples/lumen` is
  registered.
- **`register_all!`** is the explicit fallback. Reach for it for **generic
  beans** (which can't be inventoried, since the monomorphization is chosen at the
  use site) or to keep wiring local to a single focused test.

Both register the same beans against the same container; the scan just builds the
list for you from the link-time inventory.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| wiring imagined as a hand-written function | understood as **declared beans** the framework's component scan discovers and wires — no composition root to maintain |
| ports felt abstract | seen concretely as `Arc<dyn Broker>` parameters on the `ledger` `#[bean]` — one factory to swap an adapter |
| how `FireflyApplication` resolves the graph unclear | named: register infra beans → scan → resolve controllers, constructing collaborators in dependency order |

## Exercises

1. **Read the bean inventory.** Run Lumen and read the `:: beans (…) ::` block in
   the startup report. Find `LumenBeans`, `WalletApi`, and the `ledger` /
   `event_store` / `read_model` factories, grouped by stereotype — the same data
   the admin dashboard's `/beans` view renders.
2. **Add a bean and watch it appear.** Add a small `#[derive(Service)]` to
   `web.rs`, run Lumen, and confirm it shows up in the startup report — you wrote
   no registration call. Then add `#[firefly(condition_on_property =
   "wallet.enabled=true")]` and watch it disappear until you set the property.
3. **Auto-bind a port.** Define a `Clock` trait, give `SystemClock`
   `#[firefly(provides = "dyn Clock", primary)]`, and resolve `dyn Clock`. Add a
   second implementation without `primary` and observe the non-unique-bean error;
   move `primary` to fix it.
4. **Swap a store from one factory.** Change the `event_store` `#[bean]` in
   `LumenBeans` to return a different store, and explain in one sentence why the
   `ledger` factory, the handlers, and the controller need no change — they
   depend on the `EventStore` *port*.

With the wiring understood, the reactive primitives underpin everything that
follows — read [The Reactive Model](./05-reactive-model.md) next, then give
Lumen its first endpoints in [Your First HTTP API](./06-first-http-api.md).
