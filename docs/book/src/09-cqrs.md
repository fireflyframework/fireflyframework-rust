# CQRS

In [Domain-Driven Design](./08-domain-driven-design.md) Lumen's `Wallet`
aggregate learned to enforce its own rules, and the read model found a home. But
a controller still needs a way to *deliver* an instruction to the write side and
ask a *question* of the read side — and to do it without the two paths sharing a
code path, so reads can be cached and writes validated independently.

This chapter draws that bright line. It wires Lumen's command/query bus end to
end, exactly as the shipped [`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen)
crate does: four message structs, one handler bean, and the controller seam that
dispatches through the bus and keeps the read cache honest after a write.

By the end of this chapter you will:

- Explain what **Command/Query Responsibility Segregation** buys you, and how
  Firefly keeps a single typed bus while still reporting commands and queries
  apart.
- Define Lumen's `OpenWallet` / `Deposit` / `Withdraw` commands and its
  `GetWallet` query as `#[derive(Command)]` / `#[derive(Query)]` structs, with
  field-level validation and a query cache TTL generated for you.
- Write the `WalletHandlers` **handler bean** — a `#[derive(Service)]` whose
  `#[handlers]` impl carries `#[command_handler]` / `#[query_handler]` methods
  that reach their collaborators through `#[autowired]` fields.
- Understand how `FireflyApplication` drains those handlers onto a
  framework-provided `Bus` and installs the correlation, query-cache, and
  validation middleware — with no wiring code in Lumen.
- Dispatch from the controller with `bus.send` / `bus.query`, map a `CqrsError`
  to the right RFC 9457 status, and enforce read-after-write consistency by
  invalidating the cached query family after every mutation.

## Concepts you will meet

Before the first message, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — Command/Query Responsibility Segregation (CQRS).** A
> pattern that routes state-changing **commands** and read-only **queries**
> through separate handlers, so the two halves can evolve, scale, and be
> optimised independently — reads cached, writes validated. The Spring analog is
> a CQRS application split into `@CommandHandler` / `@QueryHandler` components
> (e.g. as Axon Framework names them).

> **Note** **Key term — message.** A *message* is the typed value you hand the
> bus: a command (it mutates) or a query (it reads). Every Lumen message is a
> plain serializable struct. In Spring/Axon terms a message is the command or
> query DTO you `send` or `query` through a gateway.

> **Note** **Key term — bus.** The *bus* is Firefly's command/query dispatcher.
> It matches each message to exactly one handler by `std::any::TypeId`, runs it
> through a middleware chain, and returns the handler's result. The Spring/Axon
> analog is the `CommandGateway` / `QueryGateway`, except here it is one
> in-process `Arc<Bus>` the framework provides.

> **Note** **Key term — handler bean.** A *handler bean* is an ordinary DI bean
> whose methods serve commands and queries. Its collaborators arrive by
> constructor injection, and the framework registers each method on the bus at
> boot. This is Spring's `@Component` that carries `@CommandHandler` /
> `@QueryHandler` methods.

> **Note** **Key term — middleware.** A *middleware* wraps every dispatch with
> cross-cutting behaviour — validation, caching, correlation — before and after
> the handler runs. The Spring analog is a `HandlerInterceptor` or an Axon
> `MessageHandlerInterceptor`. Firefly installs a small default chain for you.

> **Design note.** The whole path is ordinary Rust: no proxies, no reflection,
> just a typed registry keyed by `TypeId` and a method call. Lumen's handlers
> live on a DI bean (`#[derive(Service)]` + `#[handlers]`), so each reaches its
> collaborators through `self.<autowired field>`. A simpler app can write a
> handler as a free `async fn` instead — the [free-fn
> alternative](#step-4--know-the-free-fn-handler-alternative) below covers that
> form.

## Step 1 — Understand the `Message` trait

**Action.** Before writing any messages, look at the contract every command and
query satisfies. Every message implements `Message`. You will never hand-write
this impl — the derives generate it — but knowing its shape explains what the
middleware reacts to:

```rust,ignore
pub trait Message: Clone + Serialize + Send + Sync + 'static {
    fn kind() -> MessageKind { MessageKind::Command }   // Command / Query split
    fn validate(&self) -> Result<(), CqrsError> { Ok(()) }   // ValidationMiddleware
    fn cache_ttl(&self) -> Option<Duration>     { None }     // QueryCache
}
```

**What just happened.** The trait's *supertraits* state what a message must be,
and its *methods* are overridable defaults the matching middleware picks up
automatically:

- `Clone` stands in for pass-by-value handler invocation, and `Serialize` seeds
  the query-cache key (the cache hashes the message's JSON).
- `kind()` reports whether the message is a command or a query. The default is
  `MessageKind::Command`; `#[derive(Query)]` overrides it.
- `validate()` is the pre-dispatch validation hook the `ValidationMiddleware`
  calls. The default accepts everything, so a plain message passes untouched.
- `cache_ttl()` is the cache opt-in the `QueryCache` middleware reads. The
  default `None` means "not cacheable", so commands fall straight through the
  cache.

> **Note** **Key term — `MessageKind`.** A two-variant enum,
> `MessageKind::Command` / `MessageKind::Query`, that records the write/read
> nature of a message type. The bus stores each handler's kind at registration
> so it can list commands and queries separately — that is the segregation in
> "Command/Query Responsibility Segregation".

> **Tip** **Checkpoint.** You should be able to say, in one breath, what each of
> the three methods is for: `kind()` splits command from query, `validate()`
> gates dispatch, `cache_ttl()` opts a query into the cache. The rest of the
> chapter is mostly making the derives fill these in for you.

## Step 2 — Define Lumen's commands and query

**Action.** Create `src/commands.rs`. The four messages are plain structs
carrying `#[derive(Command)]` / `#[derive(Query)]`, which generate the `Message`
impl. The `#[firefly(validate)]` field attribute makes a field required (the
generated `validate()` rejects an empty `String` or a non-positive number), and
`#[firefly(cache_ttl = "...")]` is reflected on the query's generated
`cache_ttl`:

```rust,ignore
// samples/lumen/src/commands.rs
use std::sync::Arc;

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Wallet, WalletView};
use crate::ledger::{Ledger, ReadModel};
use crate::money::Money;

/// `POST /api/v1/wallets` command — open a new wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command, Builder, Schema)]
#[serde(default)]
pub struct OpenWallet {
    /// The wallet owner's display name — required.
    #[firefly(validate)]
    #[builder(into)]
    pub owner: String,
    /// The opening balance, in minor units (cents); must be `>= 0`.
    #[serde(rename = "openingBalance")]
    #[builder(default)]
    pub opening_balance: i64,
}

/// `POST /api/v1/wallets/:id/deposit` command — credit a wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct Deposit {
    /// The wallet to credit — required.
    #[firefly(validate)]
    #[serde(rename = "walletId")]
    pub wallet_id: String,
    /// The amount to credit, in minor units (cents); must be `> 0`.
    #[firefly(validate)]
    pub amount: i64,
}

/// `POST /api/v1/wallets/:id/withdraw` command — debit a wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct Withdraw {
    #[firefly(validate)]
    #[serde(rename = "walletId")]
    pub wallet_id: String,
    #[firefly(validate)]
    pub amount: i64,
}

/// `GET /api/v1/wallets/:id` query — cached for 30 seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    /// The wallet id to fetch.
    pub id: String,
}
```

**What just happened.** Three derives are doing the heavy lifting:

- `#[derive(Command)]` / `#[derive(Query)]` generate each struct's `Message`
  impl. `Command` keeps the default `kind()` of `MessageKind::Command`; `Query`
  overrides it to `MessageKind::Query`. That single difference is the whole CQRS
  split — `OpenWallet` / `Deposit` / `Withdraw` register as commands and
  `GetWallet` registers as a query, with no extra annotation.
- `#[firefly(validate)]` on a field makes it required: the generated `validate()`
  rejects an empty `String` or a non-positive number *at compile-time-generated
  code*, not by runtime reflection. On `Deposit::amount` it rejects a zero or
  negative amount before the handler ever runs, so the aggregate is never even
  called with structurally wrong data.
- `#[firefly(cache_ttl = "30s")]` on `GetWallet` is reflected on the generated
  `cache_ttl()`, which the `QueryCache` middleware reads off the message to
  memoise reads for 30 seconds.

A few choices echo the domain chapter. The commands carry `i64` cents, not a
`Money` value object — the handler constructs `Money`, keeping the wire contract
a bare number and the validation simple. And `#[serde(rename = ...)]` keeps the
JSON camelCase (`openingBalance`, `walletId`) while the Rust fields stay
snake_case.

> **Note** `OpenWallet` also derives `Builder` (Lombok's `@Builder`) and
> `Schema` (it feeds the OpenAPI docs). `Builder` gives it a fluent constructor —
> `OpenWallet::builder().owner("ada").build()` — with `opening_balance`
> defaulting to zero. Neither derive affects the CQRS behaviour; they are along
> for the ride because `OpenWallet` is also a request body.

> **Tip** **Checkpoint.** `cargo build` compiles `src/commands.rs`. The validation
> and cache behaviour is testable without a bus, because the derives put the
> methods on the type itself:
>
> ```rust,ignore
> assert!(OpenWallet::default().validate().is_err());   // empty owner rejected
> assert!(Deposit { wallet_id: "wlt_1".into(), amount: 0 }.validate().is_err());
> assert!(GetWallet::default().cache_ttl().is_some());  // the 30s TTL
> ```

## Step 3 — Write the handler bean

**Action.** Add the handler bean to `src/commands.rs`. Lumen's handlers live on a
**DI bean**, the Rust analog of a Spring `@Component` that carries
`@CommandHandler` / `@QueryHandler` methods. `WalletHandlers` is a
`#[derive(Service)]` whose collaborators — the write-side `Ledger` and the
read-side `ReadModel` — are `#[autowired]` from the container. The `#[handlers]`
impl-level macro (the CQRS sibling of `#[rest_controller]`) marks the methods:
each `#[command_handler]` / `#[query_handler]` is an `async fn(&self, msg) ->
Result<.., CqrsError>`, so a handler reaches its collaborators through `self` —
no process-global, no composition root:

```rust,ignore
// samples/lumen/src/commands.rs (continued)

/// Maps a `DomainError` onto the bus's `CqrsError` channel. The web layer
/// restores the precise HTTP status from the detail message.
fn to_cqrs(e: DomainError) -> CqrsError {
    CqrsError::handler(e.to_string())
}

/// The CQRS **handler bean** — Spring's `@Component` command/query handler. Its
/// collaborators are `#[autowired]` from the DI container; `#[handlers]`
/// registers each method on the bus.
#[derive(Service)]
struct WalletHandlers {
    /// The write-side application service (autowired).
    #[autowired]
    ledger: Arc<Ledger>,
    /// The read-side projection store the `GetWallet` query reads (autowired).
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletHandlers {
    /// Handles `OpenWallet`.
    #[command_handler]
    async fn open_wallet(&self, cmd: OpenWallet) -> Result<WalletView, CqrsError> {
        if cmd.opening_balance < 0 {
            return Err(CqrsError::validation("openingBalance must be >= 0"));
        }
        self.ledger
            .open(&cmd.owner, Money::cents(cmd.opening_balance))
            .await
            .map_err(to_cqrs)
    }

    /// Handles `Deposit`.
    #[command_handler]
    async fn deposit(&self, cmd: Deposit) -> Result<WalletView, CqrsError> {
        self.ledger
            .deposit(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }

    /// Handles `Withdraw`.
    #[command_handler]
    async fn withdraw(&self, cmd: Withdraw) -> Result<WalletView, CqrsError> {
        self.ledger
            .withdraw(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }

    /// Handles `GetWallet` — serve from the projected read model, falling back
    /// to folding the event stream when the projection has not yet caught up.
    #[query_handler]
    async fn get_wallet(&self, q: GetWallet) -> Result<WalletView, CqrsError> {
        if let Some(view) = self.read_model.find(&q.id) {
            return Ok(view);
        }
        let events = self.ledger.load_events(&q.id).await.map_err(to_cqrs)?;
        Ok(Wallet::rehydrate(&q.id, &events).view())
    }
}
```

**What just happened.** Each command handler constructs the `Money` value object
from the command's `i64`, delegates to the autowired `Ledger` application service
(which rehydrates the aggregate, runs the domain command, and persists — see
[Event Sourcing](./11-event-sourcing.md)), and maps a `DomainError` onto the
bus's `CqrsError` channel via `to_cqrs`.

The `get_wallet` query is the read-after-write pattern in miniature: it serves
from the projected `ReadModel` first, and *only* if the projection has not yet
caught up does it fall back to folding the event stream
(`Wallet::rehydrate(..).view()`). That fallback is what keeps a read immediately
after a write from returning a stale balance under the eventual consistency the
projection introduces.

> **Note** A `#[handlers]` method takes `&self` plus exactly one message argument
> and returns a `Result<.., CqrsError>`. Because the bean is a regular container
> bean, its collaborators arrive by **constructor injection** through
> `#[autowired]` fields — the same wiring every other Firefly bean uses, with no
> process-global to seed. Adding a handler is adding a method; the framework
> finds it.

**Why it matters.** Behind the macro, each `#[command_handler]` /
`#[query_handler]` submits a `BeanHandlerRegistration` into a compile-time
`inventory` registry. At boot `FireflyApplication` resolves `WalletHandlers` from
the container — wiring its `#[autowired]` `Ledger` + `ReadModel` — and installs a
bus closure that captures the resolved bean, so each dispatch calls
`self.open_wallet(..)` and friends. Lumen writes **no** registration call: the
framework drains the bean handlers for you (Step 5).

> **Tip** **Checkpoint.** `cargo build` still compiles. You can exercise the bean
> directly with no HTTP and no bus, constructing it with the same collaborators
> the container would inject:
>
> ```rust,ignore
> let handlers = WalletHandlers {
>     ledger: Arc::new(Ledger::new(
>         Arc::new(MemoryEventStore::new()),
>         Arc::new(InMemoryBroker::new()),
>     )),
>     read_model: Arc::new(ReadModel::default()),
> };
> let opened = handlers
>     .open_wallet(OpenWallet { owner: "alice".into(), opening_balance: 100 })
>     .await
>     .unwrap();
> assert_eq!(opened.balance, 100);
> ```

## Step 4 — Know the free-`fn` handler alternative

**Action.** Nothing to write for Lumen here — but it is worth knowing the second
form, because a simpler app reaches for it. A handler need not be a bean. The
free-`fn` form is the natural option for a *collaborator-free* handler (the
framework's `macro-quickstart` sample uses it): mark a free `async fn(msg) ->
Result<R, CqrsError>` with `#[command_handler]` / `#[query_handler]`:

```rust,ignore
// The simpler form — a free fn with no collaborators to inject.
#[command_handler]
pub async fn place_order(cmd: PlaceOrder) -> Result<OrderView, CqrsError> {
    Ok(OrderView::from(cmd))
}
```

**What just happened.** The macro reads the argument type (`PlaceOrder`) as the
dispatch key, generates a `register_place_order(bus)` helper, **and** submits a
`HandlerRegistration` into the `inventory` registry the framework drains — so the
free-fn handler is discovered and installed exactly like the bean form.

**Why it matters.** Because a free function can't own a `Ledger` or a
`ReadModel`, this form fits handlers that compute purely from the message (or
reach a process-global). The moment a handler needs injected collaborators — as
*all* of Lumen's do — the bean form from Step 3 is the natural fit: it gets
constructor injection for free and keeps the handler a plain method on a
`@Component`. Lumen has only bean handlers; the free-fn path drains none of its
own.

## Step 5 — Let the framework wire the bus

**Action.** Again, no wiring code to write — that is the point. The `Bus` and the
`QueryCache` are declared as `#[bean]`s in `LumenBeans` (the
`#[derive(Configuration)]` holder in `src/web.rs`), the `WalletApi` controller
autowires the `Arc<Bus>`, and `FireflyApplication` does the rest at boot. The
query cache is a plain `#[bean]` factory:

```rust,ignore
// samples/lumen/src/web.rs — LumenBeans (#[derive(Configuration)]).
#[bean]
impl LumenBeans {
    /// The read-side query cache honouring `GetWallet`'s 30s TTL (`@Bean`).
    #[bean]
    fn query_cache(&self) -> QueryCache {
        QueryCache::new()
    }
    // ... event_store, jwt_service, ledger, security beans ...
}
// The read store is *not* a `#[bean]` here — `ReadModel` is its own bean,
// registered by the scan directly.
```

**What just happened.** At boot, `FireflyApplication`:

- **Drains the discovered bean handlers** with
  `firefly::cqrs::register_discovered_handler_beans(&bus, &container)`: it
  resolves `WalletHandlers` from the container — autowiring its `Ledger` +
  `ReadModel` — and installs each `#[command_handler]` / `#[query_handler]`
  method onto the bus.
- **Drains any free-`fn` handlers** with
  `firefly::cqrs::register_discovered_handlers(&bus)`, so the two forms coexist
  (Lumen has only bean handlers, so this drains none of its own).
- **Auto-installs the bus middleware chain**: validation (installed first by the
  core), then a correlation propagator, then the `QueryCache` read-cache
  middleware whenever a `QueryCache` bean is present.

Lumen calls none of these drains. Conceptually, the framework runs:

```rust,ignore
// What FireflyApplication does for you — no Lumen code calls this.
firefly::cqrs::register_discovered_handlers(&bus);                  // free-fn handlers
firefly::cqrs::register_discovered_handler_beans(&bus, &container); // WalletHandlers' 4 methods
```

> **Note** **Where does the `Bus` come from?** It is a framework-provided
> infrastructure bean: the core registers an `Arc<Bus>` into the container before
> the scan, so the `WalletApi` controller can autowire it (`#[autowired] pub bus:
> Arc<Bus>`) and the framework can drain the discovered handlers onto it. You
> declare the *application* beans (`QueryCache`, the ledger); the bus is wired in
> for you.

**Why it matters.** The bean handlers are resolved from the *same* container that
builds the controller and the saga, so every collaborator — handler, controller,
projection — shares the one `Ledger` and one `ReadModel` the container holds.
There is no second copy of the read model to drift out of sync.

Three middleware entries ship in the dispatch chain. The framework installs them
automatically (a fourth, authorization, arrives at the HTTP edge with
[Security](./14-security.md)). Middleware runs first-registered = outermost:

| Middleware                  | Behaviour                                                      |
|-----------------------------|---------------------------------------------------------------|
| `ValidationMiddleware`      | calls `Message::validate` before dispatch, short-circuits on error — installed first by the core, so it is outermost |
| `CorrelationMiddleware`     | ensures-or-generates the correlation id for the dispatch (next step) |
| `QueryCache::middleware()`  | memoises results for messages whose `cache_ttl` is `Some` — installed when a `QueryCache` bean exists |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 380 306" role="img"
     aria-label="The CQRS bus dispatch: a message is matched to a handler by TypeId, passes the middleware chain, then reaches your handler"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <text x="190" y="20" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">send / query a message</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="190" y1="28" x2="190" y2="46"/><polygon points="190,54 186,46 194,46"/>
  </g>
  <rect x="100" y="56" width="180" height="40" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="190" y="76" text-anchor="middle" font-size="12" fill="#3a2a1c"
        font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">msg &#8614; TypeId</text>
  <text x="190" y="90" text-anchor="middle" font-size="10" fill="#7a6450">matched against registered handlers</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="190" y1="96" x2="190" y2="114"/><polygon points="190,122 186,114 194,114"/>
  </g>
  <text x="190" y="138" text-anchor="middle" font-size="11.5" font-weight="600" fill="#7a6450">middleware chain</text>
  <g>
    <rect x="118" y="148" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="141" y="176" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">V</text>
    <rect x="216" y="148" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="239" y="176" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Q</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="164" y1="170" x2="210" y2="170"/><polygon points="216,170 208,166 208,174"/>
  </g>
  <g font-size="10.5" fill="#7a6450">
    <text x="120" y="208">V = ValidationMiddleware</text>
    <text x="120" y="222">Q = QueryCache</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="190" y1="232" x2="190" y2="250"/><polygon points="190,258 186,250 194,250"/>
  </g>
  <rect x="120" y="260" width="140" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="190" y="284" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">your handler</text>
</svg>
<figcaption>A message is matched to its handler by <code>TypeId</code>, then runs the registered middleware chain (validation outermost, then the query cache) before the handler executes.</figcaption>
</figure>

> **Tip** **Checkpoint.** `cargo run` boots Lumen and the startup report's CQRS
> line counts your handlers — three commands and one query. The admin `/cqrs`
> view on the management port (`:8081`) lists them, badged blue for commands and
> green for queries.

## Step 6 — See how the bus segregates commands and queries

**Action.** Look at how the bus keeps the two halves apart, even though they share
one registry. The bus dispatches commands and queries through one registry keyed
by `TypeId`, but it does not treat them as interchangeable: each registered
handler carries the **kind** of the message it serves, exposed as `Message::kind()
-> MessageKind`:

```rust,ignore
pub enum MessageKind { Command, Query }
```

**What just happened.** The default is `MessageKind::Command`.
`#[derive(Command)]` keeps that default; `#[derive(Query)]` overrides `kind()` to
return `MessageKind::Query`. Nothing in Lumen's `src/commands.rs` changes —
`OpenWallet` / `Deposit` / `Withdraw` are already commands and `GetWallet` is
already a query, so the segregation falls out of the derives Step 2 introduced.
The bus records each message's kind at registration time and lets you ask about
the two halves separately:

```rust,ignore
use firefly::cqrs::{Bus, MessageKind};

let bus = Bus::new();
// In a unit test you populate a bus explicitly; the app boot resolves the
// `WalletHandlers` bean and drains its methods with
// `register_discovered_handler_beans(&bus, &container)`.
bus.register(|cmd: OpenWallet| async move { /* ... */ });   // three commands + one query

// Inspect the registry, split by CQRS kind.
let commands = bus.command_handler_names();      // ["...::Deposit", "...::OpenWallet", "...::Withdraw"]
let queries  = bus.query_handler_names();        // ["...::GetWallet"]
assert_eq!(bus.handler_count(), 4);

// The general form both of the above delegate to:
assert_eq!(bus.handler_names_by_kind(MessageKind::Query), queries);

// Type-level membership and removal.
assert!(bus.has_handler::<GetWallet>());
assert!(bus.unregister::<GetWallet>());          // true — one was present
assert!(!bus.has_handler::<GetWallet>());
```

`command_handler_names()` and `query_handler_names()` are thin wrappers over
`handler_names_by_kind(MessageKind)`, each returning the fully-qualified type
names sorted alphabetically — the same list `handler_names()` returns, but
filtered to one kind. `handler_count()` is the total registry size;
`has_handler::<C>()` tests membership for a message type; and `unregister::<C>()`
removes a handler, returning whether one was present (useful when a test wants to
swap a handler without rebuilding the bus).

**Why it matters.** This is exactly what the admin `/cqrs` view consumes: because
the bus knows each handler's kind, the dashboard tags every registration with a
badge (commands blue, queries green) and shows separate command/query counts,
rather than one undifferentiated handler list.

> **Note** Firefly keeps a single `Bus` and recovers the command/query split from
> each message's `kind()` (set by the `Command` / `Query` derive), rather than
> from two distinct buses. `command_handler_names()` / `query_handler_names()`
> are the filtered views the admin `/cqrs` dashboard renders; `has_handler::<C>()`
> / `unregister::<C>()` test membership and remove a handler by type.

## Step 7 — Follow the correlation id across the dispatch boundary

**Action.** Understand the middleware that keeps one logical request traceable. A
command rarely acts alone. `bus.send(Deposit { .. })` runs a handler that may
start the transfer saga ([Sagas](./12-sagas.md)) or `tokio::spawn` a follow-up
task — and each of those leaves the original request task. For the logs and
traces to read as *one* operation, they must all share a single correlation id.

> **Note** **Key term — correlation id.** A single identifier stamped on
> everything done for one logical request, so its logs and traces can be stitched
> together. Firefly threads it through a task-local; the web layer sets one per
> HTTP request. The Spring analog is the MDC `traceId` propagated by Sleuth /
> Micrometer Tracing.

`firefly::cqrs::CorrelationMiddleware` enforces that at the dispatch boundary. The
framework installs it on every `FireflyApplication` bus, between the validation
and query-cache layers, so you never wire it by hand. If you build a bus
yourself, add it like any other middleware:

```rust,ignore
use firefly::cqrs::{Bus, CorrelationMiddleware};

let bus = Bus::new();
bus.use_middleware(CorrelationMiddleware::new());   // earlier-registered = more outer
```

**What just happened.** On each dispatch the middleware **ensures-or-generates** a
correlation id: if the request is already running under one — the `firefly-web`
correlation layer sets a task-local id per HTTP request — it reuses that id, so
the command and the saga/spawned task it triggers all trace to the same value. If
no ambient id is present (a background job, a test, an internal dispatch), it
generates a fresh one for the span of that dispatch and restores the prior scope
on the way out, so sibling operations never leak ids into one another.

```rust,ignore
// Inside a handler (or anything it calls), the id is observable:
let trace = firefly_kernel::correlation_id();   // Some(<id>) under the middleware
```

**Why it matters.** On Lumen's bus the framework installs `ValidationMiddleware`
first (so it is outermost), then `CorrelationMiddleware`, then `QueryCache`. The
same id that the HTTP layer stamped on `POST /wallets/:id/deposit` flows into the
`Deposit` handler, into the transfer saga it may start, and into the events the
saga publishes — without any handler touching the id explicitly. Because
correlation sits ahead of `QueryCache` in the chain, the correlation scope is
already open before the cache layer runs, so anything the cache logs carries the
id too:

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 420 320" role="img"
     aria-label="The CQRS bus dispatch with validation outermost: a message is matched by TypeId, passes the Validation, Correlation and QueryCache chain, then reaches your handler"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <text x="210" y="20" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">send / query a message</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="28" x2="210" y2="46"/><polygon points="210,54 206,46 214,46"/>
  </g>
  <rect x="130" y="56" width="160" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="80" text-anchor="middle" font-size="12" fill="#3a2a1c"
        font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">msg &#8614; TypeId</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="94" x2="210" y2="112"/><polygon points="210,120 206,112 214,112"/>
  </g>
  <text x="210" y="136" text-anchor="middle" font-size="11.5" font-weight="600" fill="#7a6450">middleware chain</text>
  <g>
    <rect x="80" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="103" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">V</text>
    <rect x="187" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="210" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">C</text>
    <rect x="294" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="317" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Q</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="126" y1="168" x2="181" y2="168"/><polygon points="187,168 179,164 179,172"/>
    <line x1="233" y1="168" x2="288" y2="168"/><polygon points="294,168 286,164 286,172"/>
  </g>
  <g font-size="10.5" fill="#7a6450">
    <text x="80" y="208">V = ValidationMiddleware   C = Correlation   Q = QueryCache</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="222" x2="210" y2="264"/><polygon points="210,272 206,264 214,264"/>
  </g>
  <rect x="140" y="274" width="140" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="298" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">your handler</text>
</svg>
<figcaption>The framework registers <code>ValidationMiddleware</code> first (outermost), then <code>CorrelationMiddleware</code>, then <code>QueryCache</code>: the correlation scope opens before the cache layer runs, so everything it logs carries the id.</figcaption>
</figure>

> **Design note.** `CorrelationMiddleware` ensures one logical request keeps one
> correlation id across the command boundary and any saga or `tokio::spawn`ed
> continuation it triggers: it reuses an ambient id when present (the web layer
> sets one per HTTP request) and generates one otherwise, restoring the prior
> scope on the way out. Firefly threads the id through a task-local that this
> middleware scopes per dispatch, so a handler never has to pass it by hand.

## Step 8 — Dispatch from the controller

**Action.** Wire the HTTP surface to the bus. The `#[rest_controller]` (built in
[Your First HTTP API](./06-first-http-api.md)) holds the `Bus` and dispatches
through `send` / `query`. `Bus::query` is a readability synonym for `send`. A
failed dispatch is a `CqrsError`, which the web layer maps to the right RFC 9457
status:

```rust,ignore
// samples/lumen/src/web.rs — WalletApi handlers.
#[post("/wallets")]
async fn open(
    State(api): State<WalletApi>,
    Json(body): Json<OpenWallet>,
) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
    let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
    Ok((axum::http::StatusCode::CREATED, Json(view)))
}

#[get("/wallets/:id")]
async fn get(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
) -> WebResult<Json<WalletView>> {
    let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
    Ok(Json(view))
}
```

**What just happened.** `api.bus.send(body)` matches `body`'s type
(`OpenWallet`) to the `open_wallet` command handler and runs it through the
middleware chain; `api.bus.query(GetWallet { id })` does the same for the query.
The controller autowires the `Arc<Bus>` (`#[autowired] pub bus: Arc<Bus>`), so
`api.bus` already has a receiver — no hand-built state.

`cqrs_to_web` is the seam where a domain failure becomes an HTTP status. It reads
the `CqrsError` and its detail string — which, recall, is the `DomainError`'s
stable `Display` text from the previous chapter — and chooses the status:

```rust,ignore
// samples/lumen/src/web.rs
fn cqrs_to_web(err: CqrsError) -> WebError {
    match err {
        CqrsError::Validation(detail) => WebError::from(FireflyError::validation(detail)),
        CqrsError::Handler(detail) => {
            if detail.ends_with("not found") {
                WebError::from(FireflyError::not_found(detail))            // 404
            } else if detail == DomainError::InsufficientFunds.to_string()
                || detail == DomainError::NonPositiveAmount.to_string()
                || detail == DomainError::OwnerRequired.to_string()
            {
                WebError::from(FireflyError::validation(detail))           // 422
            } else {
                WebError::from(FireflyError::not_found(detail))
            }
        }
        other => WebError::from(FireflyError::internal(other.to_string())), // 500
    }
}
```

**Why it matters.** This is why the domain chapter insisted the `Display` strings
be *stable*: they are the contract `cqrs_to_web` matches on to recover the precise
status. A validation `CqrsError` becomes a 422 problem; a "not found" handler
detail becomes a 404; an insufficient-funds or non-positive-amount detail becomes
a 422; anything else falls through to a 500 — all rendered as RFC 9457
`application/problem+json`.

> **Tip** **Checkpoint.** With `cargo run` up, open a wallet and read it back:
>
> ```bash
> curl -s -XPOST localhost:8080/api/v1/wallets \
>   -H 'content-type: application/json' \
>   -d '{"owner":"alice","openingBalance":100}'
> # 201 with {"id":"...","owner":"alice","balance":100}
>
> curl -s -XPOST localhost:8080/api/v1/wallets \
>   -H 'content-type: application/json' -d '{"owner":""}'
> # 422 problem+json — the empty owner failed the #[firefly(validate)] check
> ```

## Step 9 — Keep reads fresh after a write

**Action.** Close the read-after-write gap. `GetWallet` is cached for 30 seconds.
Without care, a deposit would update the balance while a cached `GetWallet` kept
serving the old one for up to 30 seconds. Lumen invalidates the cached query
family after every mutation:

```rust,ignore
// samples/lumen/src/web.rs — deposit handler.
#[post("/wallets/:id/deposit")]
async fn deposit(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    Json(body): Json<AmountBody>,
) -> WebResult<Json<WalletView>> {
    let cmd = Deposit { wallet_id: id, amount: body.amount };
    let view: WalletView = api.bus.send(cmd).await.map_err(cqrs_to_web)?;
    api.query_cache.invalidate_type::<GetWallet>();   // read-after-write
    Ok(Json(view))
}
```

**What just happened.** This is where `WalletApi` grows the field [Your First HTTP
API](./06-first-http-api.md) deferred: alongside `bus`, the controller
**autowires** the `Arc<QueryCache>` from the container (`#[autowired] pub
query_cache: Arc<QueryCache>`), so `api.query_cache` has a receiver. The same
`QueryCache` bean the framework registers as bus middleware is the one the
controller invalidates — one cache, read by the bus and invalidated by the
handler.

`QueryCache::invalidate_type::<GetWallet>()` evicts every cached result for
exactly that query type. The withdraw handler does the same, and the transfer
saga ([Sagas](./12-sagas.md)) — which touches two wallets — invalidates the whole
`GetWallet` family.

**Why it matters.** Read-after-write consistency lives at the *bus boundary*, not
inside the handler. The handler computes the new state; the controller, having
just mutated, evicts the cache so the next `GetWallet` recomputes. The query
cache's backend swap (Redis / Postgres) and event-driven invalidation get their
own treatment in [Caching](./17-caching.md); here, the point is that a mutation
and its cache eviction sit side by side on the write path.

> **Tip** **Checkpoint.** Deposit into the wallet you opened, then read it back —
> the new balance comes through immediately, even though `GetWallet` is cached for
> 30 seconds, because the deposit handler evicted the cached entry:
>
> ```bash
> curl -s -XPOST localhost:8080/api/v1/wallets/<id>/deposit \
>   -H 'content-type: application/json' -d '{"amount":50}'
> curl -s localhost:8080/api/v1/wallets/<id>   # balance reflects the deposit
> ```

## Step 10 — Dispatch reactively (optional)

**Action.** When you want a lazy, composable result, use the bus's reactive
surface. The bus wraps the eventual result in a lazy `Mono<R>` — the same handler
lookup, the same middleware chain, run only when the `Mono` is subscribed,
blocked, or awaited. These methods take `&Arc<Bus>` so the `Mono` can own the bus:

| Method                          | Returns       |
|---------------------------------|---------------|
| `Bus::send_mono(cmd)`           | `Mono<R>`     |
| `Bus::query_mono(q)`            | `Mono<R>`     |
| `Bus::send_mono_with_context`   | `Mono<R>`     |
| `Bus::query_mono_with_context`  | `Mono<R>`     |

The `*_with_context` variants carry an explicit `ExecutionContext` into the
dispatch — the correlation id, tenant, and authenticated principal — for when a
`Mono` is composed outside the task-local scope the HTTP layer establishes (a
background job or a reactive pipeline assembled before the request context is in
play). The plain `send_mono` / `query_mono` inherit whatever context is ambient
at subscribe time.

A reactive `GetWallet`, composing on the `Mono` from [The Reactive
Model](./05-reactive-model.md):

```rust,ignore
use std::sync::Arc;
use firefly::cqrs::Bus;

let balance = bus
    .query_mono::<_, WalletView>(GetWallet { id: wallet_id })
    .map(|view| view.balance)
    .block()
    .await?;            // Some(<cents>) or None
```

**What just happened.** `query_mono` describes the dispatch without running it;
`.map(..)` composes a transformation onto the still-lazy `Mono`; `.block().await`
finally runs the chain and yields `Result<Option<i64>, FireflyError>` — `Some` on
a hit, `None` if the `Mono` completed empty.

**Why it matters.** Because `firefly-reactive` fixes its error channel to
`FireflyError`, a failed dispatch is mapped from `CqrsError` into a status-faithful
`FireflyError` (validation → 422, missing handler → 500), with the original
`CqrsError` preserved as `source()`. So a reactive command flows straight into the
RFC 9457 problem stack while staying inspectable.

## Step 11 — Prove the wiring with tests

**Action.** Lumen's `src/commands.rs` exercises the handler bean directly with no
HTTP — the test that ships in the crate. The bean operates on its `#[autowired]`
collaborators, so the test constructs it with the same `Ledger` + `ReadModel` the
container would inject and calls its methods (the full bus wiring is covered
end-to-end by the HTTP tests, which boot the whole `FireflyApplication`):

```rust,ignore
#[tokio::test]
async fn handler_bean_operates_on_its_autowired_collaborators() {
    let handlers = WalletHandlers {
        ledger: Arc::new(Ledger::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )),
        read_model: Arc::new(ReadModel::default()),
    };

    let opened = handlers
        .open_wallet(OpenWallet { owner: "alice".into(), opening_balance: 100 })
        .await
        .unwrap();
    assert_eq!(opened.balance, 100);

    let after = handlers
        .deposit(Deposit { wallet_id: opened.id.clone(), amount: 50 })
        .await
        .unwrap();
    assert_eq!(after.balance, 150);

    let fetched = handlers
        .get_wallet(GetWallet { id: opened.id.clone() })
        .await
        .unwrap();
    assert_eq!(fetched.id, opened.id);
}
```

**What just happened.** Because the handler is a plain method on a plain struct,
the test needs no bus and no DI container — just the collaborators. It opens,
deposits, and reads back, asserting the balance moves as expected.

The validation derive is testable on its own too — no bus needed, because
`#[derive(Command)]` generates `validate()` directly on the type:

```rust,ignore
#[test]
fn deposit_validates_required_fields() {
    assert!(Deposit::default().validate().is_err());
    assert!(
        Deposit { wallet_id: "wlt_1".into(), amount: 0 }.validate().is_err(),
        "zero amount fails the #[firefly(validate)] check"
    );
    assert!(Deposit { wallet_id: "wlt_1".into(), amount: 10 }.validate().is_ok());
}

#[test]
fn get_wallet_carries_cache_ttl() {
    assert!(GetWallet::default().cache_ttl().is_some());
}
```

> **Tip** **Checkpoint.** `cargo test` is green. The handler-bean test and the
> validation/cache tests pass without a running server, and the HTTP integration
> tests boot the full `FireflyApplication` to cover the bus end to end.

## Recap — what changed in Lumen

Lumen's read and write paths are now separate, typed, and bus-dispatched:

- **`src/commands.rs`** — `OpenWallet` / `Deposit` / `Withdraw` carry
  `#[derive(Command)]` with `#[firefly(validate)]` on required fields; `GetWallet`
  carries `#[derive(Query)]` with `#[firefly(cache_ttl = "30s")]`. The derives
  generate the `Message` impl, the `validate()` checks, the query's `cache_ttl`,
  and each message's `kind()` (the command/query split).
- **The `WalletHandlers` bean** (`#[derive(Service)]` + `#[handlers]`) carries the
  `#[command_handler]` / `#[query_handler]` methods and `#[autowired]`s the
  `Ledger` + `ReadModel` — a Spring `@Component` command/query handler. Command
  handlers build the `Money` value object and delegate to `self.ledger`; the query
  serves `self.read_model` and falls back to folding the stream for
  read-after-write freshness. A simpler app can write a collaborator-free handler
  as a free `async fn` instead (the same `#[command_handler]` macro applies).
- **Constructor injection, no process-global.** The handler bean reaches its
  collaborators through `#[autowired]` fields the container fills, so there is no
  `OnceLock` to seed and no `bind` step — the `ledger` `#[bean]` is a pure factory.
- **The bus** is a framework-provided bean the `WalletApi` autowires; the
  framework resolves the handler bean from the container and drains its methods
  onto the bus (`register_discovered_handler_beans`, alongside the free-`fn`
  `register_discovered_handlers`) and auto-installs the validation, correlation,
  and `QueryCache` middleware. The controller dispatches via `bus.send` /
  `bus.query`, with `cqrs_to_web` mapping a `CqrsError` (carrying the domain
  `Display` string) to the right RFC 9457 status — 422 for business rules, 404 for
  not-found.
- **Command/query segregation** falls out of the derives: one `Bus`,
  `command_handler_names()` / `query_handler_names()` filtering by `kind()`, and
  the admin `/cqrs` dashboard rendering the two halves apart.
- **Read-after-write** is enforced at the bus boundary:
  `query_cache.invalidate_type::<GetWallet>()` runs after every mutation.

You also now know that the bus exposes a reactive surface (`send_mono` /
`query_mono`, returning a lazy `Mono<R>`) whose error channel is `FireflyError`,
so a reactive dispatch flows straight into the RFC 9457 problem stack.

## Exercises

1. **Watch validation short-circuit.** In a test, build a `Bus`, add
   `ValidationMiddleware::new()`, register a `Deposit` handler closure that drives
   a `WalletHandlers` you constructed by hand, and `bus.send` a
   `Deposit { wallet_id: "wlt_1".into(), amount: 0 }`. Assert the result is a
   `CqrsError::Validation` and that the ledger was never touched (open a wallet
   first, then deposit zero, and confirm its balance is unchanged).

2. **Prove the cache, then bust it.** Against the framework-assembled router
   (`build_router().await`), `query(GetWallet { id })` twice and confirm the
   second is served from cache (instrument `ReadModel::find` or trace a counter).
   Deposit into the wallet, then `query` again — assert the new balance comes back,
   proving `invalidate_type::<GetWallet>()` did its job.

3. **Add a `CloseWallet` command.** Define `CloseWallet { #[firefly(validate)]
   wallet_id: String }` with `#[derive(Command)]`, then add a `#[command_handler]
   async fn close_wallet(&self, cmd: CloseWallet) -> Result<WalletView, CqrsError>`
   method to the `WalletHandlers` `#[handlers]` impl that returns a `WalletView`,
   and dispatch it. The framework resolves the bean and drains the new method
   automatically — you add no registration call. (You do not need a domain `close`
   yet — returning the current view is enough to exercise the wiring.)

4. **Reactive compose.** Rewrite the `get` controller handler to use
   `bus.query_mono::<_, WalletView>(GetWallet { id }).map(|v| v.balance)` and
   return just the balance as JSON. Note where the `FireflyError` channel takes
   over from `CqrsError`.

5. **Inspect the split.** In a test, register all four of Lumen's handlers on a
   `Bus`, then assert `bus.command_handler_names()` has three entries and
   `bus.query_handler_names()` has one. Confirm `bus.handler_count()` is `4` and
   that `bus.has_handler::<GetWallet>()` is `true`. This is exactly what the admin
   `/cqrs` dashboard renders.

## Where to go next

The bus dispatches *within* the service. To propagate what happened *between*
collaborators — the read-model projection, external subscribers — fan out domain
events. Continue to
**[Event-Driven Architecture & Messaging](./10-eda-messaging.md)**.

- The handlers delegate to the `Ledger`, which rehydrates the aggregate and
  persists its events — that machinery is **[Event Sourcing](./11-event-sourcing.md)**.
- A command that touches two wallets runs as a compensating saga in **[Sagas,
  Workflows & TCC](./12-sagas.md)**.
- The query cache's backend swap and event-driven invalidation get their own
  treatment in **[Caching](./17-caching.md)**.
