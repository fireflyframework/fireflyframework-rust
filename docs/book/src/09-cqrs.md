# CQRS

Lumen's `Wallet` aggregate enforces its own rules, and the read model has a
home. But the controller still needs a way to *deliver* an instruction to the
write side and a *question* to the read side — and to do it without the two
paths sharing a code path, so reads can be cached and writes can be validated
independently.

**CQRS** — Command Query Responsibility Segregation — draws that bright line.
Writes become **commands** (`OpenWallet`, `Deposit`, `Withdraw`); reads become a
**query** (`GetWallet`). Each travels a typed `Bus`, matched to its handler by
`std::any::TypeId`, through a middleware chain that validates commands and caches
queries. This chapter wires Lumen's bus end to end, exactly as `samples/lumen`
does.

> **By the end of this chapter, Lumen will** have `src/commands.rs`: the
> `OpenWallet` / `Deposit` / `Withdraw` commands and the `GetWallet` query as
> `#[derive(Command)]` / `#[derive(Query)]` structs, and a **handler bean** —
> `WalletHandlers`, a `#[derive(Service)]` whose `#[handlers]` impl carries the
> `#[command_handler]` / `#[query_handler]` methods and `#[autowired]`s the
> `Ledger` + `ReadModel` — that the framework resolves from the container and
> drains onto a bus declared as a `#[bean]`, the validation + query-cache
> middleware auto-installed, and the read-after-write cache invalidation that
> keeps a balance from going stale after a deposit.

> **Design note.** The `Bus` is Firefly's command/query dispatcher: it matches
> each message to its handler by `TypeId` and runs it through a middleware chain
> that validates commands and caches queries. A handler is an `async fn` a macro
> registers — `bus.send` / `bus.query` dispatch to it. Lumen's handlers live on a
> **DI bean** (`#[derive(Service)]` + `#[handlers]`), so each reaches its
> collaborators through `self.<autowired field>` — Spring's `@Component`
> command/query handler. (A simpler app can write the handler as a free
> `async fn` instead; the [free-fn alternative](#the-free-fn-handler-alternative)
> below covers that form.) The whole path is ordinary Rust: no proxies, no
> reflection, just a typed registry and a method call.

## Commands, queries, and the `Message` trait

Every command and query implements `Message`. Hand-writing it is one line, but
the trait's optional methods — `validate`, `cache_ttl` — are overridable
defaults that the matching middleware picks up automatically:

```rust,ignore
pub trait Message: Clone + Serialize + Send + Sync + 'static {
    fn validate(&self) -> Result<(), CqrsError> { Ok(()) }   // ValidationMiddleware
    fn cache_ttl(&self) -> Option<Duration>     { None }     // QueryCache
}
```

`Clone` stands in for pass-by-value handler invocation; `Serialize` seeds the
query cache key. Lumen never writes that impl by hand — it derives it.

## Lumen's commands and query

The four messages are plain structs carrying `#[derive(Command)]` /
`#[derive(Query)]`, which generate the `Message` impl. The `#[firefly(validate)]`
field attribute makes a field required (the generated `validate()` rejects an
empty `String` or a non-positive number), and `#[firefly(cache_ttl = "...")]` is
reflected on the query's generated `cache_ttl`:

```rust,ignore
// samples/lumen/src/commands.rs
use firefly::prelude::*;
use serde::{Deserialize, Serialize};

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

A few choices echo the domain chapter. The commands carry `i64` cents, not a
`Money` — the handler constructs the value object, keeping the wire contract a
bare number and the validation simple. `#[firefly(validate)]` on `amount`
rejects a zero or negative amount *before* the handler runs, so the aggregate is
never even called with structurally wrong data. And `#[serde(rename = ...)]`
keeps the JSON camelCase (`openingBalance`, `walletId`) while the Rust fields
stay snake_case.

> **Note.** `#[firefly(validate)]` makes a field required — the generated
> `validate()` rejects an empty `String` or a non-positive number before the
> handler runs — and the check is generated by the derive macro at compile time,
> not reflected at runtime. `#[firefly(cache_ttl = "30s")]` sets the query's
> cache TTL, which the `QueryCache` middleware picks up off the message.

## The handler bean — `#[derive(Service)]` + `#[handlers]`

Lumen's handlers live on a **DI bean**, the Rust analog of a Spring `@Component`
that carries `@CommandHandler` / `@QueryHandler` methods. `WalletHandlers` is a
`#[derive(Service)]` whose collaborators — the write-side `Ledger` and the
read-side `ReadModel` — are `#[autowired]` from the container. The `#[handlers]`
impl-level macro (the CQRS sibling of `#[rest_controller]`) marks the methods:
each `#[command_handler]` / `#[query_handler]` is an `async fn(&self, msg) ->
Result<.., CqrsError>`, so a handler reaches its collaborators through `self` —
no process-global, no composition root:

```rust,ignore
use std::sync::Arc;

use firefly::prelude::*;

use crate::domain::{DomainError, Wallet, WalletView};
use crate::ledger::{Ledger, ReadModel};
use crate::money::Money;

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

Each command handler constructs the `Money` value object from the command's
`i64`, delegates to the autowired `Ledger` application service (which rehydrates
the aggregate, runs the domain command, and persists — see
[Event Sourcing](./11-event-sourcing.md)), and maps a `DomainError` onto the
bus's `CqrsError` channel:

```rust,ignore
fn to_cqrs(e: DomainError) -> CqrsError {
    CqrsError::handler(e.to_string())
}
```

The `get_wallet` query is the read-after-write pattern in miniature: it serves
from the projected `ReadModel` first, and *only* if the projection has not yet
caught up does it fall back to folding the event stream. That fallback is what
keeps a read immediately after a write from returning a stale balance under the
eventual consistency the projection introduces.

Behind the macro, each `#[command_handler]` / `#[query_handler]` submits a
`BeanHandlerRegistration` into a compile-time `inventory` registry. At boot
`FireflyApplication` resolves `WalletHandlers` from the container — wiring its
`#[autowired]` `Ledger` + `ReadModel` — and installs a bus closure that captures
the resolved bean, so each dispatch calls `self.open_wallet(..)` and friends.
Lumen writes **no** registration call: the framework drains the bean handlers
for you (the wiring section, below).

> **Note.** A `#[handlers]` method takes `&self` plus exactly one message
> argument and returns a `Result<.., CqrsError>`. Because the bean is a regular
> container bean, its collaborators arrive by **constructor injection** through
> `#[autowired]` fields — the same wiring every other Firefly bean uses, with no
> process-global to seed. Adding a handler is adding a method; the framework
> finds it.

## The free-`fn` handler alternative

A handler need not be a bean. The free-`fn` form is the simpler option for a
collaborator-free handler (and the `macro-quickstart` sample uses it): mark a
free `async fn(msg) -> Result<R, CqrsError>` with `#[command_handler]` /
`#[query_handler]`. The macro reads the argument type as the dispatch key,
generates a `register_<fn>(bus)` helper, **and** submits a `HandlerRegistration`
into the `inventory` registry the framework drains
(`register_discovered_handlers`) — so the free-fn handler is discovered and
installed exactly like the bean form:

```rust,ignore
// The simpler form — a free fn with no collaborators to inject.
#[command_handler]
pub async fn place_order(cmd: PlaceOrder) -> Result<OrderView, CqrsError> {
    Ok(OrderView::from(cmd))
}
```

Because a free function can't own a `Ledger` or a `ReadModel`, this form fits
handlers that compute purely from the message (or reach a process-global). The
moment a handler needs injected collaborators — as all of Lumen's do — the bean
form above is the natural fit: it gets constructor injection for free and keeps
the handler a plain method on a `@Component`.

## Wiring the bus

Lumen writes **no** bus-wiring code. The `Bus` and the `QueryCache` are declared
as `#[bean]`s in `LumenBeans` (the `#[derive(Configuration)]` holder in
`src/web.rs`), the `WalletApi` controller autowires the `Arc<Bus>`, and the
framework — `FireflyApplication` — does the rest at boot:

- it drains the discovered **bean** handlers with
  `firefly::cqrs::register_discovered_handler_beans(&bus, &container)`: it
  resolves `WalletHandlers` from the container — autowiring its `Ledger` +
  `ReadModel` — and installs each `#[command_handler]` / `#[query_handler]`
  method onto the bus;
- it also drains any free-`fn` handlers with
  `firefly::cqrs::register_discovered_handlers(&bus)`, so the two forms coexist
  (Lumen has only bean handlers, so this drains none of its own);
- it auto-installs the bus middleware chain: a correlation propagator always, the
  `QueryCache` read-cache middleware whenever a `QueryCache` bean is present, and
  validation (already installed by the core).

Lumen calls none of these drains. The bean handlers are resolved from the same
container that builds the controller and the saga, so every collaborator —
handler, controller, projection — shares the one `Ledger` and one `ReadModel` the
container holds:

```rust,ignore
// What FireflyApplication does for you, conceptually — no Lumen code calls this.
firefly::cqrs::register_discovered_handlers(&bus);                 // free-fn handlers
firefly::cqrs::register_discovered_handler_beans(&bus, &container); // WalletHandlers' 4 methods
```

The bus and query cache are plain `#[bean]` factories:

```rust,ignore
// samples/lumen/src/web.rs — LumenBeans (#[derive(Configuration)]).
#[bean]
impl LumenBeans {
    /// The read-side query cache honouring `GetWallet`'s 30s TTL (`@Bean`).
    #[bean]
    fn query_cache(&self) -> QueryCache {
        QueryCache::new()
    }
    // ... event_store, read_model, jwt_service, ledger, security beans ...
}
```

> **Where does the `Bus` come from?** It is a framework-provided infrastructure
> bean: the core registers an `Arc<Bus>` into the container before the scan, so the
> `WalletApi` controller can autowire it (`#[autowired] pub bus: Arc<Bus>`) and the
> framework can drain the discovered handlers onto it. You declare the
> *application* beans (`QueryCache`, the ledger); the bus is wired in for you.

Middleware runs first-registered = outermost. Two app-visible entries ship in this
chain — and the framework installs them automatically (a third, authorization,
arrives at the HTTP edge with [Security](./14-security.md)):

| Middleware                  | Behaviour                                                      |
|-----------------------------|---------------------------------------------------------------|
| `QueryCache::middleware()`  | memoises results for messages whose `cache_ttl` is `Some` — installed when a `QueryCache` bean exists |
| `ValidationMiddleware`      | calls `Message::validate` before dispatch, short-circuits on error |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 380 306" role="img"
     aria-label="The CQRS bus dispatch: a message is matched to a handler by TypeId, passes the QueryCache and ValidationMiddleware chain, then reaches your handler"
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
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Q</text>
    <rect x="216" y="148" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="239" y="176" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">V</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="164" y1="170" x2="210" y2="170"/><polygon points="216,170 208,166 208,174"/>
  </g>
  <g font-size="10.5" fill="#7a6450">
    <text x="120" y="208">Q = QueryCache</text>
    <text x="120" y="222">V = ValidationMiddleware</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="190" y1="232" x2="190" y2="250"/><polygon points="190,258 186,250 194,250"/>
  </g>
  <rect x="120" y="260" width="140" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="190" y="284" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">your handler</text>
</svg>
<figcaption>A message is matched to its handler by <code>TypeId</code>, then runs the registered middleware chain (here <code>QueryCache</code> then <code>ValidationMiddleware</code>) before the handler executes.</figcaption>
</figure>

## Command/query segregation

The bus dispatches commands and queries through one registry keyed by `TypeId`,
but it does not treat them as interchangeable: each registered handler carries
the **kind** of the message it serves. That kind is a property of the message
type, exposed as `Message::kind() -> MessageKind`:

```rust,ignore
pub enum MessageKind { Command, Query }
```

The default is `MessageKind::Command`. `#[derive(Command)]` keeps that default;
`#[derive(Query)]` overrides `kind()` to return `MessageKind::Query`. Nothing in
Lumen's `src/commands.rs` changes — `OpenWallet` / `Deposit` / `Withdraw` are
already commands and `GetWallet` is already a query, so the segregation falls out
of the derives the chapter introduced. The bus records each message's kind at
registration time and lets you ask about the two halves separately:

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

This is exactly what the admin `/cqrs` view consumes: because the bus now knows
each handler's kind, the dashboard tags every registration with a badge (commands
blue, queries green) and shows separate command/query counts, rather than one
undifferentiated handler list.

> **Note.** Firefly keeps a single `Bus` and recovers the command/query split
> from each message's `kind()` (set by the `Command` / `Query` derive), rather
> than from two distinct buses. `command_handler_names()` /
> `query_handler_names()` are the filtered views the admin `/cqrs` dashboard
> renders; `has_handler::<C>()` / `unregister::<C>()` test membership and remove
> a handler by type.

## Correlation propagation

A command rarely acts alone. `bus.send(Deposit { .. })` runs a handler that may
start the transfer saga ([Sagas](./12-sagas.md)) or `tokio::spawn` a follow-up
task — and each of those leaves the original request task. For the logs and
traces to read as *one* operation, they must all share a single correlation id.

`firefly::cqrs::CorrelationMiddleware` enforces that at the dispatch boundary.
The framework installs it on every `FireflyApplication` bus as the outermost
middleware, before the query-cache and validation layers, so you never wire it by
hand. If you build a bus yourself, add it like any other middleware:

```rust,ignore
use firefly::cqrs::{Bus, CorrelationMiddleware};

let bus = Bus::new();
bus.use_middleware(CorrelationMiddleware::new());   // outermost — runs first
```

On each dispatch the middleware **ensures-or-generates** a correlation id: if the
request is already running under one — the `firefly-web` correlation layer sets a
task-local id per HTTP request — it reuses that id, so the command and the
saga/spawned task it triggers all trace to the same value. If no ambient id is
present (a background job, a test, an internal dispatch), it generates a fresh
one for the span of that dispatch and restores the prior scope on the way out, so
sibling operations never leak ids into one another.

```rust,ignore
// Inside a handler (or anything it calls), the id is observable:
let trace = firefly_kernel::correlation_id();   // Some(<id>) under the middleware
```

Because the framework installs `CorrelationMiddleware` outermost on Lumen's bus,
the same id that the HTTP layer stamped on `POST /wallets/:id/deposit` flows into
the `Deposit` handler, into the transfer saga it may start, and into the events
the saga publishes — without any handler touching the id explicitly. It sits
outermost in the chain shown earlier — the correlation scope is already open
before `QueryCache` and `ValidationMiddleware` run, so anything they log carries
the id too:

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 420 320" role="img"
     aria-label="The CQRS bus dispatch with correlation outermost: a message is matched by TypeId, passes the Correlation, QueryCache and ValidationMiddleware chain, then reaches your handler"
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
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">C</text>
    <rect x="187" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="210" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Q</text>
    <rect x="294" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="317" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">V</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="126" y1="168" x2="181" y2="168"/><polygon points="187,168 179,164 179,172"/>
    <line x1="233" y1="168" x2="288" y2="168"/><polygon points="294,168 286,164 286,172"/>
  </g>
  <g font-size="10.5" fill="#7a6450">
    <text x="80" y="208">C = Correlation   Q = QueryCache   V = ValidationMiddleware</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="222" x2="210" y2="264"/><polygon points="210,272 206,264 214,264"/>
  </g>
  <rect x="140" y="274" width="140" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="298" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">your handler</text>
</svg>
<figcaption>Registering <code>CorrelationMiddleware</code> first puts it outermost: the correlation scope opens before <code>QueryCache</code> and <code>ValidationMiddleware</code> run, so everything they log carries the id.</figcaption>
</figure>

> **Design note.** `CorrelationMiddleware` ensures one logical request keeps one
> correlation id across the command boundary and any saga or `tokio::spawn`ed
> continuation it triggers: it reuses an ambient id when present (the web layer
> sets one per HTTP request) and generates one otherwise, restoring the prior
> scope on the way out. Firefly threads the id through a task-local that this
> middleware scopes per dispatch, so a handler never has to pass it by hand.

## Dispatching from the controller

The `#[rest_controller]` (built in [First HTTP API](./06-first-http-api.md))
holds the `Bus` and dispatches through `send` / `query`. `Bus::query` is a
readability synonym for `send`. A failed dispatch is a `CqrsError`, which the web
layer maps to the right RFC 9457 status:

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

`cqrs_to_web` is the seam where a domain failure becomes an HTTP status. It reads
the `CqrsError` and its detail string — which, recall, is the `DomainError`'s
stable `Display` text from the previous chapter — and chooses the status:

```rust,ignore
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

This is why the domain chapter insisted the `Display` strings be *stable*: they
are the contract `cqrs_to_web` matches on to recover the precise status.

## Read-after-write: invalidating the cache

`GetWallet` is cached for 30 seconds. Without care, a deposit would update the
balance while a cached `GetWallet` kept serving the old one for up to 30 seconds.
Lumen closes that gap by invalidating the cached query family after every
mutation:

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

This is where `WalletApi` finally grows the second field [First HTTP
API](./06-first-http-api.md) deferred: alongside `bus`, the controller
**autowires** the `Arc<QueryCache>` from the container (`#[autowired] pub
query_cache: Arc<QueryCache>`), so `api.query_cache` has a receiver. The same
`QueryCache` bean the framework registers as bus middleware is the one the
controller invalidates — one cache, read by the bus and invalidated by the
handler.

`QueryCache::invalidate_type::<GetWallet>()` evicts every cached result for
exactly that query type. The withdraw handler does the same, and the transfer
saga ([Sagas](./12-sagas.md)) — which touches two wallets — invalidates the
whole `GetWallet` family. The query cache's backend swap (Redis / Postgres) and
event-driven invalidation get their own treatment in [Caching](./17-caching.md);
here, the point is that the *bus* is where read-after-write consistency lives,
not the handler.

## The reactive bus

The bus also exposes a reactive surface that wraps the eventual result in a lazy
`Mono<R>` — the same handler lookup, the same middleware chain, run only when the
`Mono` is subscribed, blocked, or awaited. The methods take `&Arc<Bus>` so the
`Mono` can own the bus:

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

A reactive `GetWallet`, composing on the `Mono` from
[The Reactive Model](./05-reactive-model.md):

```rust,ignore
use std::sync::Arc;
use firefly::cqrs::Bus;

let balance = bus
    .query_mono::<_, WalletView>(GetWallet { id: wallet_id })
    .map(|view| view.balance)
    .block()
    .await?;            // Ok(Some(<cents>))
```

Because `firefly-reactive` fixes its error channel to `FireflyError`, a failed
dispatch is mapped from `CqrsError` into a status-faithful `FireflyError`
(validation → 422, missing handler → 500), with the original `CqrsError`
preserved as `source()`. So a reactive command flows straight into the RFC 9457
problem stack while staying inspectable.

## Proving the handler bean

Lumen's `src/commands.rs` exercises the handler bean directly with no HTTP — the
test that ships in the crate. The bean operates on its `#[autowired]`
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

And the validation derive is testable on its own — no bus needed, because
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

## What changed in Lumen

Lumen's read and write paths are now separate, typed, and bus-dispatched:

- **`src/commands.rs`** — `OpenWallet` / `Deposit` / `Withdraw` carry
  `#[derive(Command)]` with `#[firefly(validate)]` on required fields;
  `GetWallet` carries `#[derive(Query)]` with `#[firefly(cache_ttl = "30s")]`.
  The derives generate the `Message` impl, the `validate()` checks, and the
  query's `cache_ttl`.
- **The `WalletHandlers` bean** (`#[derive(Service)]` + `#[handlers]`) carries the
  `#[command_handler]` / `#[query_handler]` methods and `#[autowired]`s the
  `Ledger` + `ReadModel` — a Spring `@Component` command/query handler. Command
  handlers build the `Money` value object and delegate to `self.ledger`; the query
  serves `self.read_model` and falls back to folding the stream for
  read-after-write freshness. A simpler app can write a collaborator-free handler
  as a free `async fn` instead (the same `#[command_handler]` macro applies to
  free functions).
- **Constructor injection, no process-global.** The handler bean reaches its
  collaborators through `#[autowired]` fields the container fills, so there is no
  `OnceLock` to seed and no `bind` step — the `ledger` `#[bean]` is now a pure
  factory.
- **The bus** is a framework-provided bean the `WalletApi` autowires; the
  framework resolves the handler bean from the container and drains its methods
  onto the bus (`register_discovered_handler_beans`, alongside the free-`fn`
  `register_discovered_handlers`) and auto-installs the correlation,
  `QueryCache`, and validation middleware. The controller dispatches via
  `bus.send` / `bus.query`, with `cqrs_to_web` mapping a `CqrsError` (carrying the
  domain `Display` string) to the right RFC 9457 status — 422 for business rules,
  404 for not-found.
- **Read-after-write** is enforced at the bus boundary:
  `query_cache.invalidate_type::<GetWallet>()` runs after every mutation.

## Exercises

1. **Watch validation short-circuit.** In a test, build a `Bus`, add
   `ValidationMiddleware::new()`, register a `Deposit` handler closure that drives
   a `WalletHandlers` you constructed by hand, and `bus.send` a
   `Deposit { wallet_id: "wlt_1".into(), amount: 0 }`. Assert the result is a
   `CqrsError::Validation` and that the ledger was never touched (open a wallet
   first, then deposit zero, and confirm its balance is unchanged).

2. **Prove the cache, then bust it.** Against the framework-assembled router
   (`build_router().await`), `query(GetWallet { id })` twice and confirm the
   second is served from cache (instrument the `ReadModel::find` or trace a
   counter). Deposit into the wallet, then `query` again — assert the new balance
   comes back, proving `invalidate_type::<GetWallet>()` did its job.

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

The bus dispatches *within* the service. To propagate what happened *between*
collaborators — the read-model projection, external subscribers — fan out domain
events. Continue to
[Event-Driven Architecture & Messaging](./10-eda-messaging.md).
