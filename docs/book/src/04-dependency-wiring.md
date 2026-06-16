# Dependency Wiring

In the [Quickstart](./02-quickstart.md) you wrote a one-line `main` and watched
`FireflyApplication::new("lumen").run()` boot a whole service. One line in that
boot pipeline did something a service usually hand-rolls: it assembled the
**object graph** — it constructed the cache, the CQRS bus, the event store, the
ledger, and the controller that depends on all of them, in the right order, and
connected them. This chapter is about *how*.

The short answer is that you never write that assembly. In a Firefly service you
**declare** each collaborator as a bean — a struct with a stereotype derive, or a
factory method — mark its dependencies `#[autowired]`, and the framework's
**component scan** discovers every declaration at boot and wires the graph for
you. There is no hand-written composition root, no `build_app`, no list of
`new(...)` calls threaded through a function. You say *what* each piece needs;
the container supplies it.

We will learn this the way the rest of the book teaches — against Lumen's real
beans, the ones in `samples/lumen`. By the end you will be able to read Lumen's
wiring, add to it, and explain exactly what the framework did at boot. The very
next chapter, [Dependency Injection & Auto-Configuration](./04a-dependency-injection.md),
then surveys the full container surface in depth; this chapter gives you the
working mental model it builds on.

By the end of this chapter you will:

- Explain what a **bean**, a **stereotype**, and a **component scan** are, and how
  they replace a hand-written composition root.
- Declare beans two ways — a stereotype derive on a struct you own, and a
  `#[bean]` factory for things you don't — and know when to reach for each.
- Use `#[autowired]` to inject a single dependency, a whole collection, an
  optional one, or a deferred `Provider`.
- Bind a trait to its implementation with `provides`, and disambiguate several
  candidates with `primary` and `order`.
- Read Lumen's bean inventory in the startup report and trace how
  `FireflyApplication` resolves the graph from the declarations.

## Concepts you will meet

Before the first declaration, here are the four ideas this chapter leans on. Each
is reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — bean.** A *bean* is an object the framework constructs and
> manages for you, then hands to whoever declares it as a dependency. You declare
> beans; the container discovers them at startup and connects them. This is
> exactly Spring's notion of a bean managed by the application context.

> **Note** **Key term — dependency injection (DI).** *Dependency injection* means
> a component does not construct its own collaborators — it declares *what* it
> needs and the framework supplies them. The piece that does the supplying is the
> **DI container**. Firefly's container is the Rust analog of Spring's
> `ApplicationContext`.

> **Note** **Key term — stereotype.** A *stereotype* is a derive you put on a
> struct to make it a managed bean and to record its architectural role —
> business logic, data access, HTTP layer, and so on. Firefly's five stereotypes
> (`Service`, `Component`, `Repository`, `Configuration`, `Controller`) mirror
> Spring's `@Service`, `@Component`, `@Repository`, `@Configuration`, and
> `@Controller`.

> **Note** **Key term — component scan.** A *component scan* is the startup pass
> that finds every declared bean and registers it. Spring scans the classpath
> with reflection; Rust has no runtime reflection, so Firefly's scan is
> *link-time* — each stereotype derive emits a registration that the scan collects
> from the compiled binary.

## Step 1 — See the wiring you no longer write

Open `samples/lumen/src/web.rs` and read its module doc comment. It calls the
file "the **composition root**", and then immediately tells you there is no
hand-written one.

> **Note** **Key term — composition root.** The *composition root* is the single
> place in a program where the object graph is assembled — where every component
> is constructed and connected. In many frameworks you write this function by
> hand. In Firefly the framework *is* the composition root: it scans your beans
> and wires them, so you never spell the graph out.

Recall the one-line `main` from the Quickstart:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

That single call assembles Lumen's entire object graph. Inside `run()`, the
wiring happens in three moves:

1. It builds the web stack and **auto-registers** the framework's own
   infrastructure beans — the CQRS `Bus`, the event `Broker`, the cache, the
   metric registry, the scheduler — into the container, so *your* beans can
   autowire them.
2. It **component-scans** the crate graph: every stereotype-derived type and every
   `#[bean]` factory in Lumen is discovered, condition-checked, and registered.
3. It **resolves** the controllers in order to mount them, which recursively
   constructs each controller's autowired collaborators in dependency order —
   exactly the graph a hand-written root would build, but derived from the
   declarations instead of spelled out.

What just happened: nothing in your code names the order in which the cache, bus,
store, ledger, and controller are built. You declared each one next to itself;
the container computed the order from the dependency types. That is the whole
trick, and the rest of the chapter is the mechanics behind it.

> **Tip** **Checkpoint.** Open `samples/lumen/src/web.rs` and find the comment
> that says there is "**no hand-written composition root and no builder**." Every
> example below comes from this file (and its sibling `ledger.rs` and
> `commands.rs`). You are reading the real wiring, not a toy.

## Step 2 — Declare a bean you own with a stereotype

The simplest bean is a struct you can annotate directly. You make it visible to
the container by deriving a **stereotype**. Lumen's read model is exactly this
case — an in-memory map the projection writes and the `GetWallet` query reads:

```rust,ignore
// src/ledger.rs — the CQRS query side, a scanned data-access bean.
use std::collections::HashMap;
use std::sync::Mutex;
use firefly::prelude::*;

/// The in-memory read model — a `#[derive(Repository)]` bean (Spring's
/// `@Repository`): the projection upserts it, `GetWallet` reads it.
#[derive(Debug, Default, Repository)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}
```

What just happened, block by block:

- `#[derive(Repository)]` is the stereotype. It declares `ReadModel` a managed
  bean *and* records its role as the data-access layer. That single derive is all
  the registration there is — no `register(...)` call, no entry in a list.
- `Default` lets the container construct the bean with zero arguments. A
  stereotype-derived struct with no `#[autowired]` fields is built by its
  `Default`, then registered as a **singleton** (one shared instance for the
  process).
- The struct's own fields (`rows`) are ordinary state. Only fields you mark
  `#[autowired]` — and `ReadModel` has none — are filled from the container.

The five stereotypes differ only in the architectural role they communicate; all
five register the type as a managed bean:

| Derive                     | Role                                                   |
|----------------------------|--------------------------------------------------------|
| `#[derive(Service)]`       | Business-logic layer: use-case orchestration.          |
| `#[derive(Component)]`     | Generic managed bean with no specific role.            |
| `#[derive(Repository)]`    | Data-access layer: databases, external storage, ports. |
| `#[derive(Configuration)]` | A factory holder that can carry `#[bean]` methods.     |
| `#[derive(Controller)]`    | HTTP layer (`#[rest_controller]` builds on this).      |

> **Design note.** The role each stereotype records is not cosmetic. It is stored
> on the bean, so the admin dashboard's `/beans` view (and the startup report) can
> group beans by layer — `[repository] ReadModel`, `[service] WalletHandlers`, and
> so on — the same DI introspection Spring Boot Actuator exposes.

> **Tip** **Checkpoint.** `ReadModel` becomes a bean from one derive and one
> `Default`. Keep that picture: *a stereotype derive is the registration.*

## Step 3 — Inject dependencies with `#[autowired]`

A read model has no collaborators, but most beans do. To declare what a bean
needs, mark a field `#[autowired]` and the container fills it in by type. This is
the Rust spelling of constructor injection: you declare *what*, the container
supplies it. Lumen's wallet controller is the textbook case (the real `WalletApi`
from `src/web.rs`):

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
    pub bus: Arc<Bus>,                 // the CQRS bus it dispatches through
    #[autowired]
    pub ledger: Arc<Ledger>,           // the application service the saga + stream use
    #[autowired]
    pub query_cache: Arc<QueryCache>,  // invalidated after a mutation
}
```

> **Note** **Key term — `Arc<T>`.** `Arc` is Rust's atomically reference-counted
> shared pointer. The container hands out shared singletons, so an injected
> dependency arrives as `Arc<T>` — many beans can hold the same `Arc<Ledger>` and
> all see the one instance. Wherever you see `#[autowired] field: Arc<T>`, read it
> as "give me the shared `T`."

What just happened: when the container constructs `WalletApi`, it first resolves
the `Bus`, then the `Ledger` (recursively building *its* own dependencies — you
will see those in Step 5), then the `QueryCache`, and only then injects all three
and hands back the controller. You wrote no constructor; the field types *are* the
constructor signature.

A dependency that does not exist surfaces as a clear **resolution error at
startup** — a named "no such bean" pointing at the missing type — not a panic
three frames deep at runtime. Fail-fast wiring is the whole point.

`#[autowired]` injects more than a single `Arc<T>`. The field's *shape* selects
the injection mode:

- `#[autowired] widgets: Vec<Arc<Widget>>` injects **every** registered `Widget`,
  ordered by each bean's `order` — collection injection, the way you gather all
  implementations of a port.
- `#[autowired] maybe: Option<Arc<Thing>>` injects `Some` when a `Thing` is
  registered and `None` when it is not — an optional dependency that does not abort
  startup if absent.
- `#[autowired] tickets: Provider<Ticket>` injects a **deferred** handle:
  `tickets.get()` resolves a fresh value on each call, the way you reach for a
  transient inside a singleton.

> **Note** **Key term — `Provider<T>`.** A `Provider<T>` is a lazy handle to a
> bean rather than the bean itself. Calling `tickets.get()` resolves it on demand.
> It is the Rust analog of Spring's `ObjectProvider` / `Provider<T>`, and the way
> a long-lived singleton pulls a short-lived bean each time it needs one.

> **Tip** **Checkpoint.** `WalletApi` names three collaborators and constructs
> none of them. If you removed the `#[autowired] ledger` line, the controller
> would no longer ask for a ledger — the field is the entire request.

## Step 4 — Declare beans you do not own with `#[bean]` factories

Not every collaborator is a struct you can put a derive on. The event store, the
query cache, the JWT service, and the ledger are all *built* — they take
constructor arguments, or they come from a third-party crate, or a factory is
simply the clearest way to express them. For these, you declare a
`#[derive(Configuration)]` holder and give it `#[bean]` factory methods.

> **Note** **Key term — `#[bean]` factory.** A `#[bean]` method is a factory: the
> container calls it and registers whatever it returns as a bean, keyed by the
> method's **return type**. The holder carries `#[derive(Configuration)]`. This is
> Spring's `@Configuration` class with `@Bean` methods, one-for-one.

Here is Lumen's whole `LumenBeans` holder from `src/web.rs`:

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

    /// The ledger application service — a **pure factory** whose parameters are
    /// **autowired**: the container resolves the event store and the
    /// framework-provided `Broker` port by type, then hands them to the factory.
    #[bean]
    fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

What just happened, block by block:

- `#[derive(Configuration)] struct LumenBeans;` is the holder. The scan discovers
  it the same way it discovers `ReadModel` — one derive.
- `#[bean] impl LumenBeans { ... }` marks the whole impl block as carrying bean
  factories, and each inner `#[bean]` method is registered as its own bean. The
  container keys each one by its return type: `event_store` registers a
  `MemoryEventStore`, `query_cache` registers a `QueryCache`, and so on.
- `event_store`, `query_cache`, and `jwt_service` take only `&self` — they are
  zero-dependency factories. The container calls them and registers the result.
- `ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>)` is the
  important one: its **parameters are themselves resolved from the container** by
  type before the method runs. A bean can depend on a bean. The container builds
  the `MemoryEventStore` (from the `event_store` factory above) and supplies the
  framework's `Broker`, then calls `ledger`.

> **Note** **Key term — port and adapter.** A *port* is an interface (in Rust, a
> trait object like `Arc<dyn Broker>` or `Arc<dyn EventStore>`); an *adapter* is a
> concrete implementation of it. "Depend on the port, inject the adapter" means a
> bean asks for the trait and the container supplies whatever implementation is
> registered. This is the hexagonal-architecture vocabulary the rest of the book
> uses.

Notice the `ledger` factory widens `Arc<MemoryEventStore>` to `Arc<dyn
EventStore>` before handing it to `Ledger::new`. The `Ledger` stores the *port*,
not the concrete store — so swapping `MemoryEventStore` for a Postgres-backed
store is a one-line change in this factory, and the ledger, the handlers, and the
controller never notice.

Three ideas carry the whole design. Read them slowly; everything later is a
consequence:

- **The framework does the registration.** You never call `register_arc` or
  `Container::bind`. `container.scan()` discovers the `LumenBeans` holder *and*
  each `#[bean]` method and registers the produced values keyed by return type.
- **Ports are resolved by type.** The `ledger` factory takes `broker: Arc<dyn
  Broker>` and the container supplies the framework's broker — "depend on the
  interface, inject the implementation."
- **A bean can depend on a bean.** `ledger(&self, store, broker)` pulls in two
  other beans by type; the container builds them first, then calls the factory —
  the same dependency-ordered construction a hand-written root would do, derived
  from the parameter types.

> **Design note.** A `#[derive(Configuration)]` holder with `#[bean]` methods is
> the Spring `@Configuration` + `@Bean` analog: a factory whose methods produce
> beans keyed by return type, resolving their own arguments from the container.
> Lumen declares its whole domain graph this way, and the component scan turns the
> declarations into the wired object graph.

> **Tip** **Checkpoint.** You now have both ways to declare a bean: a stereotype
> derive on a struct you own (`ReadModel`), and a `#[bean]` factory for things you
> build (`event_store`, `ledger`). Lumen uses a derive when it can annotate the
> type and a factory when it cannot.

## Step 5 — Trace one resolution end to end

Put Steps 2–4 together by following a single resolution: how does
`FireflyApplication` construct `WalletApi`?

1. The scan has already registered every bean: `LumenBeans` and its five
   factories, the `ReadModel`, the `WalletHandlers` and `WalletProjection` service
   beans, and `WalletApi` itself — plus the framework's own `Bus`, `Broker`,
   cache, scheduler, and registries.
2. To mount the controller, the container calls `resolve::<WalletApi>()`. The
   field types say it needs `Arc<Bus>`, `Arc<Ledger>`, and `Arc<QueryCache>`.
3. `Arc<Bus>` and `Arc<QueryCache>` already exist (the framework pre-registered
   the bus; the `query_cache` factory produced the cache). They are handed over
   directly.
4. `Arc<Ledger>` does not exist yet, so the container calls the `ledger` factory.
   That factory needs `Arc<MemoryEventStore>` and `Arc<dyn Broker>`. The container
   builds the store from the `event_store` factory, supplies the framework broker,
   and calls `ledger` — producing the `Ledger`.
5. With all three collaborators in hand, the container constructs `WalletApi` and
   caches it as a singleton.

What just happened: the container built the graph **leaves-first** — store and
broker before ledger, ledger before controller — purely from the dependency
types. That ordering is the work a composition root used to do by hand. Here it is
*derived*, and it is recomputed correctly the moment you add or remove a
dependency.

The same recursion wires the rest of Lumen. The CQRS handler bean autowires the
ledger and the read model the same way (the real `WalletHandlers` from
`src/commands.rs`):

```rust,ignore
/// The CQRS handler bean — Spring's `@Component` command/query handler. Its
/// collaborators are `#[autowired]` from the DI container.
#[derive(Service)]
struct WalletHandlers {
    #[autowired]
    ledger: Arc<Ledger>,
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletHandlers {
    #[command_handler]
    async fn deposit(&self, cmd: Deposit) -> Result<WalletView, CqrsError> {
        self.ledger
            .deposit(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }
    // ... open_wallet, withdraw, get_wallet ...
}
```

The same `Arc<Ledger>` and `Arc<ReadModel>` singletons are injected here that the
controller and projection also hold — one instance each, shared by every bean that
asks for it. (The `#[handlers]` / `#[command_handler]` machinery that puts these
methods on the bus is the subject of [CQRS & Messaging](./09-cqrs.md); for now,
notice only that a handler is a bean and gets its collaborators by autowiring.)

> **Tip** **Checkpoint.** Trace it in your head once more: `WalletApi` →
> `Ledger` → `MemoryEventStore` + `Broker`. If you can name that chain, you
> understand the resolver.

## Step 6 — Use the framework's infrastructure beans by type

You may have noticed `WalletApi` autowires `Arc<Bus>` and the `ledger` factory
autowires `Arc<dyn Broker>`, yet Lumen never *declares* a bus or a broker. They
come from the framework. Before the scan runs, `FireflyApplication` pre-registers
its own infrastructure beans into the container, so any of your beans can autowire
them by type:

| Bean (resolve by type)    | Type                                            |
|---------------------------|--------------------------------------------------|
| `Bus`                     | `Arc<cqrs::Bus>` (validation pre-installed)      |
| cache adapter             | `Arc<dyn cache::Adapter>` (Memory by default)    |
| broker                    | `Arc<dyn eda::Broker>` (InMemory by default)     |
| scheduler                 | `Arc<scheduling::Scheduler>`                     |
| metric registry           | `Arc<actuator::MetricRegistry>`                  |
| health composite          | `Arc<actuator::HealthComposite>`                |

What just happened: the container *is* Lumen's composition root, and the
framework seeds it with these collaborators first. That is why a `#[bean]` factory
can take `broker: Arc<dyn Broker>` and just receive one — the broker was already
registered. You reach any of these by autowiring it into a bean; you tune the
*configuration* knobs underneath them (CORS, idempotency, security headers, bind
addresses) through `FireflyApplication::configure`:

```rust,ignore
firefly::FireflyApplication::new("lumen")
    .configure(|cfg: &mut CoreConfig| {
        // adjust CoreConfig / WebStack knobs here
    })
    .run()
    .await
```

> **Note** **Key term — auto-configuration.** *Auto-configuration* is the
> framework pre-registering sensible infrastructure beans (an in-memory broker, an
> in-memory cache, the metric registry) so your app works with zero wiring, while
> still letting you override any of them. It is the mechanism behind Spring Boot's
> "it just works" defaults, covered in full in [Dependency Injection &
> Auto-Configuration](./04a-dependency-injection.md).

> **Tip** **Checkpoint.** No bean in `samples/lumen` constructs a `Bus`, a
> `Broker`, or a cache — they autowire them. Grep `samples/lumen/src` for
> `Arc<Bus>` and `Arc<dyn Broker>` and confirm every use is a consumer, never a
> producer.

## Step 7 — Bind a trait to its implementation

So far every autowired dependency has been a concrete type or a framework port.
When *you* own both a trait (a port) and its implementation (an adapter), you bind
them on the derive with `provides`, then resolve the trait — "depend on the port,
get the adapter":

```rust,ignore
trait Clock: Send + Sync { fn now(&self) -> u64; }

#[derive(Component, Default)]
#[firefly(provides = "dyn Clock", primary)]
struct SystemClock;
impl Clock for SystemClock { fn now(&self) -> u64 { 42 } }

// elsewhere: c.resolve::<dyn Clock>() yields the SystemClock instance.
```

What just happened, block by block:

- `#[derive(Component, Default)]` registers `SystemClock` as a managed bean, as
  usual.
- `#[firefly(provides = "dyn Clock")]` *additionally* binds the `dyn Clock` trait
  object to this implementation. Now a bean can autowire `Arc<dyn Clock>` and the
  container hands it the `SystemClock`.
- `primary` marks this the default when several beans satisfy the same trait.

> **Note** **Key term — `primary` and `order`.** When several beans satisfy one
> trait, `#[firefly(... primary)]` picks the one that plain `resolve::<dyn
> Trait>()` returns (Spring's `@Primary`), and `#[firefly(order = N)]` sets the
> position a bean takes when *all* of them are collected — by `resolve_all::<dyn
> Trait>()` or by a `Vec<Arc<...>>` autowired field (Spring's `@Order`).

`provides` on the derive is the **scan-friendly** way to bind a trait. When you
are assembling a container by hand instead (in a focused test, say), the equivalent
move is an explicit `Container::bind::<dyn Trait, Concrete>()` call; both register
the same trait-to-adapter mapping. For the rare case where you need a *specific*
named instance rather than any satisfying one, the container also supports
qualifier-by-name resolution. All three — `bind`, named beans, and the full
disambiguation surface — are covered in
[Dependency Injection & Auto-Configuration](./04a-dependency-injection.md).

Lumen itself uses `provides` for its feature-gated streaming endpoint, which it
registers as a `RouteContributor` port the framework discovers and merges:

```rust,ignore
#[cfg(feature = "streaming")]
#[derive(Service)]
#[firefly(provides = "dyn firefly::web::RouteContributor")]
struct StreamingRoutes {
    #[autowired]
    api: Arc<WalletApi>,
}
```

> **Tip** **Checkpoint.** You can now bind a port to an adapter without a
> composition root: `provides` on the derive, then `resolve::<dyn Trait>()`. Add a
> second implementation without `primary` and resolving the trait becomes a
> *non-unique-bean* error — the container refusing to guess.

## Step 8 — Gate beans by condition and profile

One codebase has to run with cheap in-memory adapters in development and real
infrastructure in production. The mechanism is **conditional registration**: a
bean can declare the circumstances under which it should exist at all, and the
scan honors that as it collects each registration.

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

What just happened: `condition_on_property = "feature.audit=on"` registers
`AuditService` only when that config property is set; `profile = "prod"` registers
`PostgresHealthCheck` only when the `prod` profile is active. The scan evaluates
these as it discovers each bean, so the container ends up holding *exactly* the
beans the environment calls for — no `if` in your service code.

> **Note** **Key term — profile.** A *profile* is a named environment slice —
> `dev`, `test`, `prod` — that toggles which beans and which configuration are
> active. Firefly reads the active profiles from configuration (the
> `FIREFLY_PROFILE` environment variable by default); profiles are introduced in
> [Configuration](./03-configuration.md) and used here to gate beans, exactly as
> Spring's `@Profile` does.

The same gating works on `#[bean]` factories — `#[bean(profile = "prod")]`
registers a factory only under the `prod` profile — and it is the engine behind
every "swap the adapter for production" callout in this book. Lumen can stay
in-memory for teaching while a production deployment flips to real infrastructure
through configuration alone.

> **Tip** **Checkpoint.** Add `#[firefly(condition_on_property =
> "wallet.enabled=true")]` to a throwaway `#[derive(Service)]` bean, run Lumen, and
> watch it *not* appear in the startup report until you set the property. The
> condition decided the bean's existence before construction.

## Step 9 — Hook into a bean's lifecycle

Real infrastructure beans need to *act* once wired — open a pool, subscribe to a
topic — and undo it on shutdown. Name the methods on the derive:

```rust,ignore
#[derive(Service, Default)]
#[firefly(post_construct = "started", pre_destroy = "stopped")]
struct ProjectionSubscriber { /* ... */ }

impl ProjectionSubscriber {
    fn started(&mut self) { /* subscribe the read-model projection */ }
    fn stopped(&self)     { /* drain and unsubscribe */ }
}
```

What just happened: `post_construct = "started"` names a method to run *after*
the bean is built and its `#[autowired]` fields are injected; `pre_destroy =
"stopped"` names a method to run on `container.destroy()`. Destruction happens in
**reverse construction order**, so a subscriber started after the store is torn
down before it — clean teardown without a hand-written shutdown sequence.

> **Note** **Key term — `post_construct` / `pre_destroy`.** These are the Rust
> analogs of Spring's `@PostConstruct` and `@PreDestroy` (and JSR-250's lifecycle
> callbacks): a method to run once after wiring completes, and a method to run on
> shutdown, with the reverse-order guarantee.

> **Tip** **Checkpoint.** Lifecycle hooks are how a bean does its one-time wiring
> *itself*, instead of a composition root doing it after construction. The
> container owns the ordering.

## Step 10 — Read the bean inventory at boot

You have declared beans, autowired them, bound a port, gated some, and given one
lifecycle hooks. The framework prints exactly what it wired. Run Lumen and read
the startup report:

```bash
cargo run
```

The `:: beans (…) ::` block lists every registered bean grouped by stereotype:
`LumenBeans` and its factories, `WalletApi`, the `ledger` / `event_store` beans,
the `[repository] ReadModel`, the `[service] WalletHandlers` and
`WalletProjection`. This is the same data the admin dashboard's `/beans` view
renders, on the management port at `http://localhost:8081/admin/`.

> **Note** **Key term — component scan (link-time).** Because Rust has no runtime
> reflection, each stereotype derive emits an `inventory` registration at compile
> time, and `firefly::scan(&container)` (equivalently `container.scan()`) collects
> every one linked into the binary and registers them — honoring conditions and
> profiles as it goes. `FireflyApplication` runs this scan for you at boot.

What just happened: the report *is* the inventory the scan produced. Nothing is
reflective or hidden — "what is wired" is printed line-by-line. A missing
dependency would have aborted the boot with a named resolution error before this
report ever printed.

> **Warning** Link-time discovery has one Rust-specific wrinkle. A bean is
> discoverable only when its crate's registrations are **linked into the binary**.
> For a single-crate app like Lumen that is automatic. But in a multi-crate
> service, a *layer* crate the binary only depends on transitively — a `-models` or
> `-core` crate whose beans are never named directly — can be **dead-stripped** by
> the linker, beans and all. Force-link those with
> [`firefly::link!`](./22-layered-microservices.md) at the binary's crate root
> (`firefly::link!(my_core, my_models);`) and guard the result with
> `firefly::assert_discovered(...)`. Single-crate Lumen never needs this; the
> note is here so the report's emptiness for a stripped crate is never a mystery.

> **Tip** **Checkpoint.** The `:: beans ::` block names every bean you declared in
> this chapter, with no registration call anywhere in your code. That is the
> payoff: you wrote declarations, the framework wrote the graph.

## The one escape hatch — `register_all!`

Component scanning is the path Lumen and the framework take, and it is the default
for everything in `samples/lumen`. There is exactly one explicit fallback, for the
two cases the scan cannot reach:

```rust,ignore
let c = Container::new();
firefly::register_all!(&c, [ReadModel, Ledger, WalletApi]);
let api = c.resolve::<WalletApi>().expect("controller resolves");
```

Reach for `register_all!` for **generic beans** — a generic type's
monomorphization is chosen at the use site, so it can't be inventoried — or simply
to keep wiring local to a single focused test. Both register the same beans
against the same container; the scan just builds the list for you from the
link-time inventory. The lower-level entry point underneath the scan is the
`ApplicationContext`, which wraps the container with the full startup sequence and
is handy in a test:

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

The error taxonomy is precise: a missing bean, a non-unique bean with no
`primary`, and a detected circular dependency each surface as a distinct, named
error at resolution time — the data the admin `/beans` view also reports.
[Dependency Injection & Auto-Configuration](./04a-dependency-injection.md) covers
the full container surface — scopes, named beans, `bind`, `register_all!`, and the
error model — in depth.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| wiring imagined as a hand-written composition root | understood as **declared beans** the component scan discovers and wires — no root to maintain |
| a stereotype derive looked decorative | seen as **the registration itself**: one derive makes a managed bean and records its layer |
| `#[autowired]` felt like a single-value annotation | known as four injection modes — `Arc<T>`, `Vec<Arc<T>>`, `Option<Arc<T>>`, `Provider<T>` |
| ports felt abstract | seen concretely as `Arc<dyn Broker>` / `Arc<dyn EventStore>` — one `#[bean]` factory to swap an adapter |
| how `FireflyApplication` resolves the graph was unclear | named: pre-register infra beans → scan → resolve controllers, building collaborators leaves-first in dependency order |

You also now know:

- Why a Firefly service has no `build_app` — declarations plus a component scan
  replace the hand-written graph, and the framework *is* the composition root.
- That conditions and profiles gate a bean's existence, so one codebase runs
  in-memory in dev and on real infrastructure in prod without an `if`.
- That `post_construct` / `pre_destroy` give a bean its own one-time wiring and
  teardown, ordered by the container.
- That `register_all!` and `ApplicationContext::builder()` are the explicit
  fallbacks for generics and focused tests — everything else is scanned.

## Exercises

1. **Read the bean inventory.** Run Lumen and read the `:: beans (…) ::` block in
   the startup report. Find `LumenBeans`, `WalletApi`, the `ledger` /
   `event_store` factories, and the `[repository] ReadModel` data-access bean,
   grouped by stereotype — the same data the admin dashboard's `/beans` view
   renders at `http://localhost:8081/admin/`.
2. **Add a bean and watch it appear.** Add a small `#[derive(Service)]` to
   `web.rs`, run Lumen, and confirm it shows up in the report — you wrote no
   registration call. Then add `#[firefly(condition_on_property =
   "wallet.enabled=true")]` and watch it disappear until you set the property.
3. **Auto-bind a port.** Define a `Clock` trait, give `SystemClock`
   `#[firefly(provides = "dyn Clock", primary)]`, and resolve `dyn Clock`. Add a
   second implementation *without* `primary`, observe the non-unique-bean error,
   then move `primary` to the one you want as the default.
4. **Swap a store from one factory.** Change the `event_store` `#[bean]` in
   `LumenBeans` to return a different store, and explain in one sentence why the
   `ledger` factory, the handlers, and the controller need no change — they depend
   on the `EventStore` *port*, not the concrete store.
5. **Trace a resolution.** Pick `WalletHandlers` and write down, in order, every
   bean the container must build before it can construct that handler. Check your
   answer against the `#[autowired]` fields in `src/commands.rs` and the `ledger`
   factory in `src/web.rs`.

## Where to go next

- Go deep on the container in **[Dependency Injection &
  Auto-Configuration](./04a-dependency-injection.md)** — scopes, named beans and
  qualifiers, `Container::bind`, the full conditional surface, and the
  auto-configuration model this chapter only sketched.
- See exactly what `run()` does, stage by stage, in **[Bootstrapping with
  FireflyApplication](./04b-bootstrap.md)** — the boot pipeline that drives the
  scan you just learned.
- Then meet the reactive primitives every later chapter builds on in **[The
  Reactive Model — Mono & Flux](./05-reactive-model.md)**, and give Lumen its
  first endpoints in **[Your First HTTP API](./06-first-http-api.md)**.
