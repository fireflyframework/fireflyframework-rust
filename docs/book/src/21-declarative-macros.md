# Declarative Services with Macros

Lumen is finished. Over twenty chapters it grew from an empty scaffold into a
secure, observable, event-sourced CQRS service with a transfer saga and a
streaming endpoint — and it depends on exactly **one** Firefly crate. This
capstone re-reads the whole service through a single lens: the **declarative
macros**. By the end of this chapter you will be able to point at every
`#[derive(...)]` and `#[...]` in `samples/lumen` and say precisely what wiring it
collapsed into a declaration next to the code. That is the thesis the running
crate proves: *one facade + macros = the framework, with the boilerplate gone.*

> **Compile-time, not reflective.** Firefly's declarative layer generates wiring
> at compile time with `proc-macro`s — there is no startup scanning cost and no
> reflective surprises. A declaration sits next to the code it describes, and the
> macro emits the `impl`s, routers, and helper functions you would otherwise
> hand-write. If you have used a batteries-included framework before, the shape
> will feel familiar; the difference is that the glue is generated and checked by
> the compiler rather than discovered at startup.

## One dependency, one prelude

Every chapter began the same way. Lumen's `Cargo.toml` lists one Firefly crate:

```toml
[dependencies]
firefly = { workspace = true }   # the whole framework + every macro
axum    = { workspace = true }   # you author the controller handlers
serde   = { workspace = true }   # your messages + event payloads
serde_json = { workspace = true }
tokio   = { workspace = true }
uuid    = { workspace = true }
chrono  = { workspace = true }
async-trait = { workspace = true }
```

and every module opens with one glob:

```rust,ignore
use firefly::prelude::*;
```

That glob brings the whole framework into scope — `Bus`, `Container`,
`Scheduler`, `Saga`/`Step`, `Application`/`ShutdownHandle`, `Core`/`CoreConfig`,
`WebResult`/`WebError`, `FireflyError`/`FireflyResult`, `Mono`/`Flux` — **and**
every macro. There are also per-crate aliases (`firefly::cqrs::Bus`,
`firefly::eventsourcing::EventStore`, `firefly::security::JwtService`, …) for the
types you name explicitly. `axum` and `serde` are the only two ecosystem crates
Lumen writes against directly.

### Staying lean

The default `firefly` build pulls only the framework's lean *port* crates — no
heavy third-party drivers. Lumen needs none, so its build is minimal. Heavy
adapters are opt-in cargo features (the swap path every chapter pointed at):

| Feature | Pulls in |
|---------|----------|
| `data-sqlx` | relational repository adapter (Postgres / MySQL / SQLite) |
| `data-mongodb` | document repository adapter (MongoDB) |
| `eda-kafka` / `eda-rabbitmq` / `eda-redis` / `eda-postgres` | event-broker transports |
| `cache-redis` / `cache-postgres` | cache backends |
| `admin` | the admin dashboard |
| `full` | all of the above |

## The macros Lumen uses

Lumen exercises the full declarative set. Here is the catalogue, each mapped to
the exact Lumen file it lands in:

| Macro | Lumen file | Generates |
|-------|-----------|-----------|
| `#[derive(Command)]` / `#[derive(Query)]` | `commands.rs` | the `Message` impl (`#[firefly(validate)]`, `#[firefly(cache_ttl = "…")]`) |
| `#[command_handler]` / `#[query_handler]` | `commands.rs` | a `register_<fn>(bus)` helper |
| `#[derive(DomainEvent)]` | `domain.rs` | `EVENT_TYPE` + `to_domain_event` |
| `#[derive(AggregateRoot)]` | `domain.rs` | `AGGREGATE_TYPE` + `aggregate()` / `aggregate_mut()` |
| `#[event_listener(topic = "…")]` | `ledger.rs` | a `subscribe_<fn>(broker)` helper |
| `#[rest_controller]` + `#[get/post]` | `web.rs` | a `routes(state) -> axum::Router` |
| `#[scheduled(fixed_rate = "…")]` | `housekeeping.rs` | a `schedule_<fn>(scheduler)` helper |
| `#[firefly::saga]` + `#[saga_step]` | `transfer.rs` | `TransferSaga::saga()` + a `run`-style step graph with compensation |
| `#[firefly::workflow]` + `#[workflow_step]` | `compliance.rs` | a workflow `run` over the DAG of steps |
| `#[firefly::tcc]` + `#[participant]` | `tcc_transfer.rs` | a TCC `run` driving each participant's try / confirm / cancel |

Lumen also exercises the declarative orchestration macros above
(`#[firefly::saga]` / `#[firefly::workflow]` / `#[firefly::tcc]`) in its own
source. Several more macros round out the declarative set; Lumen does not
exercise these in its own source — it is event-sourced (so the relational
`#[firefly::repository]` / `#[firefly::transactional]` never appear) and handles
the remaining cross-cutting concerns by other means — but each is a first-class
part of the framework, shown here as a focused standalone example:

| Macro | Purpose | Generates |
|-------|---------|-----------|
| `#[derive(Builder)]` | a fluent constructor with required/defaulted fields | `T::builder()` → fluent setters → `build() -> Result<T, String>` |
| `#[derive(Mapper)]` | compile-time struct-to-struct conversion | one compile-time `From<Source>` per `#[firefly(from = "…")]` |
| `#[firefly::repository]` | derived-query and custom-query method bodies | method bodies on a `SqlxReactiveRepository` impl from method names or `#[query(…)]` |
| `#[firefly::transactional]` | a declared transaction boundary | a commit-on-`Ok` / rollback-on-`Err` boundary around an `async fn` body |
| `#[firefly::pre_authorize]` / `#[firefly::post_authorize]` | method-level access control | an access check before the body, or a returnObject check after it |
| `#[derive(Validate)]` (+ `Valid<T>`) | JSR-380 bean validation | `impl Validate` running the field `#[validate(email/url/not_empty/length/range/pattern/custom)]` checks; the `Valid<T>` web extractor rejects a constraint failure with 422 |
| `#[cacheable]` / `#[cache_put]` / `#[cache_evict]` | declarative caching | a read-through / write-through / evict body around the process-registered cache adapter |
| `#[async_method]` | fire-and-forget async | rewrites an `async fn(self: Arc<Self>, …) -> R` into a non-async `fn … -> TaskHandle<R>` that spawns the body on the registered executor |
| `#[application_event_listener]` / `#[transactional_event_listener]` | in-process events | an `@EventListener` / `@TransactionalEventListener` discovered via `inventory` and fired by `publish_event` (the latter bound to a transaction commit phase) |
| `#[aspect]` (+ `#[before]`/`#[after]`/`#[around]`/…) | aspect-oriented advice | `impl firefly_aop::Aspect` + an `inventory` registration; advice runs around the explicit `advised(…)` weave point |

The DI stereotype derives (`#[derive(Component/Service/Repository/Configuration/
AutoConfiguration/Controller)]`, `#[bean]`, `#[autowired]`, `register_all!`) and
`#[derive(ConfigProperties)]` round out the set. `#[derive(AutoConfiguration)]`
is the auto-config holder whose `#[bean]`s back off behind a
`condition_on_missing_bean`, so an application can override any default by
declaring its own bean of the same type; `Container::scan()` auto-registers every
`#[bean]` method, and `Container::scan_packages([..])` restricts discovery to the
named module paths.
Lumen wires its collaborators explicitly rather than through the container
(chapter 4), so the [DI deep-dive](./04a-dependency-injection.md) is where you
saw those at work.

### CQRS — messages and handlers (`commands.rs`)

`#[derive(Command)]` / `#[derive(Query)]` generate the `Message` impl.
`#[firefly(validate)]` makes an empty / zero field fail validation before the
handler runs; `#[firefly(cache_ttl = "…")]` feeds the query cache. These are
verbatim Lumen:

```rust,ignore
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct OpenWallet {
    #[firefly(validate)]
    pub owner: String,
    #[serde(rename = "openingBalance")]
    pub opening_balance: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    pub id: String,
}
```

`#[command_handler]` / `#[query_handler]` mark a free `async fn` and generate a
`register_<fn>(bus)` helper that installs it on the bus:

```rust,ignore
#[command_handler]
pub async fn open_wallet(cmd: OpenWallet) -> Result<WalletView, CqrsError> { /* … */ }

#[query_handler]
pub async fn get_wallet(q: GetWallet) -> Result<WalletView, CqrsError> { /* … */ }
// generated: register_open_wallet(&bus);  register_get_wallet(&bus);
```

Lumen calls each generated helper in one `register(&bus)` fn. Because free fns
cannot capture wiring state, the resolved `Ledger` + `ReadModel` are published
once through a `bind` / `OnceLock` and reached from the handlers — the pattern
chapter 9 introduced.

> **What it generates.** `#[derive(Command)]` / `#[derive(Query)]` emit the
> `Message` impl; `#[command_handler]` / `#[query_handler]` emit a typed
> `register_<fn>(bus)` helper that installs the handler. `#[firefly(cache_ttl)]`
> exposes a TTL the query cache reads. The `Bus` is the command/query gateway
> every dispatch flows through.

### Event sourcing — domain events and the aggregate (`domain.rs`)

`#[derive(DomainEvent)]` stamps each payload with a stable `EVENT_TYPE`
discriminator (its struct name) and a `to_domain_event` conversion;
`#[derive(AggregateRoot)]` finds the embedded `AggregateRoot` field and generates
`Wallet::AGGREGATE_TYPE` plus `aggregate()` / `aggregate_mut()`:

```rust,ignore
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct WalletOpened {
    pub wallet_id: String,
    pub owner: String,
    pub opening_balance: i64,
}

#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    pub root: AggregateRoot,   // the framework root — uncommitted-event buffer + version
    pub owner: String,
    pub balance: Money,
    pub opened: bool,
}
```

The only event-sourcing wiring Lumen writes by hand is the `apply` fold; the
discriminators and the wire conversion are generated.

> **What it generates.** `#[derive(DomainEvent)]` emits a stable `EVENT_TYPE`
> discriminator plus a `to_domain_event` conversion; `#[derive(AggregateRoot)]`
> emits `AGGREGATE_TYPE` and the `aggregate()` / `aggregate_mut()` accessors over
> the embedded root. The discriminator pins each event's identity in a stable,
> versioned JSON wire format, so persisted streams stay readable as the schema
> evolves.

### Messaging — the projection listener (`ledger.rs`)

`#[event_listener(topic = "…")]` marks a free `async fn(Event) -> FireflyResult<()>`
and generates a `subscribe_<fn>(broker)` helper that subscribes it to the topic.
Lumen's read-model projection is one declaration:

```rust,ignore
#[event_listener(topic = "wallets.events")]
pub async fn project_wallet_event(ev: Event) -> FireflyResult<()> {
    // reload the wallet's stream, fold to a WalletView, upsert — idempotent.
    Ok(())
}
// generated: subscribe_project_wallet_event(broker)
```

The composition root calls `ledger::subscribe_project_wallet_event(broker)` after
binding, closing the CQRS loop.

> **What it generates.** `#[event_listener(topic = "…")]` emits a
> `subscribe_<fn>(broker)` helper that subscribes the function to the topic on
> whatever broker transport is wired in. You write only the handler body; the
> subscription wiring is generated.

### Web — the controller (`web.rs`)

`#[rest_controller(path = "…")]` turns an `impl` block into a generated
`WalletApi::routes(state) -> axum::Router`. Each method carries one verb mapping
and uses ordinary axum extractors, returning `WebResult<T>` so a handler error
renders as RFC 9457 `application/problem+json`:

```rust,ignore
#[rest_controller(path = "/api/v1")]
impl WalletApi {
    #[post("/wallets")]
    async fn open(State(api): State<WalletApi>, Json(body): Json<OpenWallet>)
        -> WebResult<(axum::http::StatusCode, Json<WalletView>)> { /* dispatch via api.bus */ }

    #[get("/wallets/:id")]
    async fn get(State(api): State<WalletApi>, Path(id): Path<String>)
        -> WebResult<Json<WalletView>> { /* … */ }
    // … deposit / withdraw / transfer
}
// generated: WalletApi::routes(state) -> axum::Router
```

The macro also submits each route into a link-time table, so the OpenAPI
generator and the actuator `/mappings` endpoint can enumerate Lumen's routes
without re-parsing the source.

> **What it generates.** `#[rest_controller]` + `#[get]`/`#[post]` emit a
> `WalletApi::routes(state) -> axum::Router` plus a link-time mapping-table entry
> per route. `WebResult<T>` renders any handler error as an
> `application/problem+json` body (RFC 9457), so error shaping is uniform across
> every endpoint without per-handler code.

### Scheduling — the heartbeat (`housekeeping.rs`)

`#[scheduled(...)]` generates a `schedule_<fn>(scheduler)` helper that registers a
zero-argument `async fn` on a `Scheduler`:

```rust,ignore
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> { /* … */ }
// generated: schedule_ledger_heartbeat(&scheduler)
```

> **What it generates.** `#[scheduled(...)]` emits a `schedule_<fn>(scheduler)`
> helper that registers a zero-argument `async fn` on a `Scheduler`. Use
> `fixed_rate = "60s"` for a fixed cadence (with an optional `initial_delay`), or
> `cron = "…"` for a cron expression.

### Construction — the fluent builder (`#[derive(Builder)]`)

Rust's stdlib derives already cover value-object boilerplate — debug formatting,
cloning, structural equality, defaults — with `#[derive(Debug, Clone, PartialEq,
Default)]` over `pub` fields. The one ergonomic gap they leave is a *fluent
builder* — and that is what `#[derive(Builder)]` fills. It generates `T::builder()` returning a
`TBuilder` with one setter per field and a `build() -> Result<T, String>`. By
default every field is **required**: `build` returns an `Err` naming the first
unset field. `#[builder(default)]` falls back to `Default::default()`,
`#[builder(default = "expr")]` to a custom expression, and `#[builder(into)]`
makes the setter accept `impl Into<FieldTy>`:

```rust,ignore
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command, Builder)]
#[serde(default)]
pub struct OpenWallet {
    #[firefly(validate)]
    #[builder(into)]                                 // accept &str, String, …
    pub owner: String,
    #[serde(rename = "openingBalance")]
    #[builder(default)]                              // unset → 0
    pub opening_balance: i64,
}

// A build-style constructor, errors surfaced as a String:
let cmd = OpenWallet::builder()
    .owner("ada")            // impl Into<String>
    .opening_balance(10_000)
    .build()?;               // Result<OpenWallet, String>
```

A required field left unset surfaces as a `build()` error, not a compile error —
the tradeoff a fluent builder makes against a hand-written typed constructor,
where the compiler enforces arity. Reach for `#[derive(Builder)]` when a struct
has many optional/defaulted fields; keep a plain literal when every field is
required and present.

> **What it generates.** `#[derive(Builder)]` emits `T::builder()` returning a
> `TBuilder` with one setter per field and a `build() -> Result<T, String>` that
> errors on the first unset required field. `#[builder(default)]` falls back to
> `Default::default()`, `#[builder(default = "expr")]` to a custom expression, and
> `#[builder(into)]` makes a setter accept `impl Into<FieldTy>`. Returning a
> `Result` keeps missing-field handling on the normal `?` path rather than a panic.

### Conversion — the compile-time mapper (`#[derive(Mapper)]`)

`#[derive(Mapper)]` generates a compile-time, type-checked `From<Source>` that
maps a source struct to a target struct field-by-field. One
`#[firefly(from = "Source")]` produces one `From` impl, and the attribute is
**repeatable** to map from several sources. Per-field attributes adjust the
mapping: `#[firefly(rename = "src")]` reads a differently named source field,
`#[firefly(into)]` applies `.into()` to the source value, `#[firefly(with = "fn")]`
runs a conversion function, and `#[firefly(default)]` /
`#[firefly(default_expr = "expr")]` fill a target field with no source read:

```rust,ignore
// A read-model view assembled from the domain aggregate.
#[derive(Debug, Clone, Serialize, Deserialize, Mapper)]
#[firefly(from = "Wallet")]
pub struct WalletView {
    #[firefly(rename = "root", with = "aggregate_id")]  // read src.root, run aggregate_id(..)
    pub id: String,
    pub owner: String,                        // same name on both ends: a plain move
    #[firefly(with = "Money::cents_value")]   // src.balance: Money -> i64 via a conversion fn
    pub balance: i64,
    #[firefly(default)]                       // version is set by the projector, not the fold
    pub version: i64,
}
// generates: impl From<Wallet> for WalletView { fn from(src: Wallet) -> Self { … } }

let view: WalletView = wallet.into();
```

Because the generated code is a plain `From` impl, every field is checked by the
compiler and there is no runtime cost — that compile-time guarantee is the whole
point. Contrast this with the **runtime** `firefly_data::Mapper`, which converts
via two serde passes and is checked at runtime: use the runtime mapper as a
dynamic fallback when the source type is not known at compile time (e.g. mapping
arbitrary JSON), and prefer `#[derive(Mapper)]` whenever both ends are concrete
types — which, in Lumen's projection, they are.

> **What it generates.** `#[derive(Mapper)]` emits one `impl From<Source> for
> Target` per `#[firefly(from = "Source")]` (the attribute is repeatable).
> Per-field attributes shape each move: `#[firefly(rename = "src")]` reads a
> differently named source field, `#[firefly(into)]` applies `.into()`,
> `#[firefly(with = "fn")]` runs a conversion function, and `#[firefly(default)]` /
> `#[firefly(default_expr = "expr")]` fill a target field with no source read. The
> runtime `firefly_data::Mapper` is the dynamic, serde-based fallback for when the
> source type isn't known until runtime.

### Persistence — derived queries and transactions (relational)

These two macros sit on the relational persistence path. Lumen's read model is
an in-memory projection over an event stream (chapter 7), so it uses neither — but
in a relational service they are the everyday tools, so here is each in brief,
with the [persistence chapter](./07-persistence.md) as the full reference.

`#[firefly::repository]` turns a `find_by_…` / `count_by_…` / `exists_by_…` /
`delete_by_…` method name into a working query body. Applied to an `impl` block, it
parses each method name into a derived query, marshals the typed arguments, and
delegates to the tested runtime engine — so you declare a typed method and get a
working, compiler-checked implementation. The runtime method is chosen from the
**return type** (`Vec<T>`/`Option<T>` → find, `i64` → count, `bool` → exists,
`u64` → delete), and the type exposes its backing `SqlxReactiveRepository` through
an accessor (default `self.repository()`, overridable with `#[repository(repo =
"…")]`):

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    async fn find_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }
    async fn find_by_iban(&self, iban: &str)     -> Result<Option<Account>, DataError> { unimplemented!() }
    async fn count_by_owner(&self, owner: &str)  -> Result<i64, DataError>          { unimplemented!() }
    async fn exists_by_email(&self, email: &str) -> Result<bool, DataError>         { unimplemented!() }
}
```

The placeholder `unimplemented!()` bodies are discarded and replaced by the
generated delegations.

**Paged derived queries.** Give a `find_by_…` method a trailing `Pageable`
argument (and a `Result<Vec<T>, DataError>` return) and the generated body appends
the page's sort and window to the query, delegating to the runtime
`SqlxReactiveRepository::find_by_derived_paged`:

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    async fn find_by_owner(&self, owner: &str, page: Pageable)
        -> Result<Vec<Account>, DataError> { unimplemented!() }
}

// Build the page (1-based index) with sort + window:
let page = Pageable::of(1, 20, RequestSort::of([Order::desc("id")]));
let rows = repo.find_by_owner("ada", page).await?;
```

**Custom queries with `#[query(…)]`.** When a name-derived query isn't enough,
annotate a stub method with `#[query(...)]` and write the statement directly.
Native SQL binds each `:name` placeholder to the argument named `name`; the
**return type** selects the operation exactly as for derived queries — `Vec<T>` /
`Option<T>` is a list, `i64` a count, `bool` an existence check, and `u64` a
modifying statement (INSERT/UPDATE/DELETE, returning affected rows):

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    #[query("SELECT id, owner FROM accounts WHERE status = :status ORDER BY id DESC")]
    async fn active_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }

    #[query("UPDATE accounts SET status = :status WHERE id = :id")]
    async fn set_status(&self, id: &str, status: &str) -> Result<u64, DataError> { unimplemented!() }
}
```

`#[query(sql = "…")]` is the explicit spelling of the native form, and
`#[query(jpql = "…", entity = "Account")]` writes the statement against entity
names. Each lowers to the matching runtime call —
`query_list` / `query_count` / `query_exists` / `query_execute`.

`#[firefly::transactional]` wraps an `async fn`'s body in a transaction governed by
the registered `TransactionManager` — commit on `Ok`, roll back on `Err` — so the
boundary becomes a declaration. The function must be `async`, must return a
`Result<T, E>`, and its error type must implement
`From<firefly_transactional::TxError>` so begin/commit failures surface through the
normal `?` path. Bare or with options (`propagation`, `isolation`, `read_only`,
`timeout_ms`):

```rust,ignore
#[firefly::transactional]
async fn open_account(repo: &AccountRepo, acct: Account) -> Result<(), DataError> {
    repo.insert(&acct).await?;     // committed together on Ok,
    repo.insert_audit(&acct).await?;  // rolled back together on Err
    Ok(())
}

#[firefly::transactional(propagation = "requires_new", isolation = "serializable", read_only = false, timeout_ms = 5000)]
async fn reconcile(repo: &LedgerRepo) -> Result<(), DataError> { /* … */ }
```

These two are the relational counterpart to how Lumen achieves consistency
*without* a transaction manager: it appends events to the `EventStore` under
optimistic concurrency and projects them, rather than mutating rows inside a
`#[transactional]` boundary (chapter 11). Same goal — atomic, consistent writes —
reached by two different architectures.

> **What it generates.** `#[firefly::repository]` replaces each stub body with a
> delegation to `SqlxReactiveRepository`: name-derived methods to
> `find_by_derived` / `find_by_derived_paged`, and `#[query(…)]` methods to
> `query_list` / `query_count` / `query_exists` / `query_execute`, picking the call
> from the return type. `#[firefly::transactional]` wraps the body in a
> begin/commit/rollback boundary on the registered `TransactionManager`;
> `propagation` / `isolation` / `read_only` / `timeout_ms` tune that boundary.

### Method security — `#[pre_authorize]` / `#[post_authorize]`

Two macros enforce access control at the method boundary, reading the caller's
identity from the ambient security context rather than from a `Request`. The full
treatment is in the [security chapter](./14-security.md); here is the catalogue
entry.

`#[firefly::pre_authorize(...)]` runs an access check **before** the function body.
Apply it to a `fn` returning `Result<T, E>` whose error type implements
`From<firefly_security::SecurityError>`, so a denial surfaces through the normal
`?` path. The rule forms are:

```rust,ignore
#[firefly::pre_authorize]                              // `authenticated` — any caller in scope
async fn whoami() -> Result<Profile, AppError> { /* … */ }

#[firefly::pre_authorize(role = "ADMIN")]              // a single role
async fn close_books(&self) -> Result<(), AppError> { /* … */ }

#[firefly::pre_authorize(any_role = ["TELLER", "ADMIN"])]
async fn open_account(&self, req: OpenAccount) -> Result<Account, AppError> { /* … */ }

#[firefly::pre_authorize(authority = "wallet:write")]  // a single fine-grained authority
async fn deposit(&self, id: &str, cents: i64) -> Result<(), AppError> { /* … */ }

#[firefly::pre_authorize(any_authority = ["wallet:write", "wallet:admin"])]
async fn withdraw(&self, id: &str, cents: i64) -> Result<(), AppError> { /* … */ }
```

When no caller is in scope the body is skipped and the macro returns
`Err(SecurityError::Unauthenticated.into())`; when a caller is present but lacks
the required role/authority it returns `Err(SecurityError::Forbidden.into())`.

`#[firefly::post_authorize(<bool expr>)]` runs **after** an `async fn` returns
`Result<T, E>` and gates the value on a boolean expression. The expression sees
`result` (a `&T` to the returned value — the returnObject) and `auth` (a
`&Authentication`); if it evaluates to `false` the value is discarded and the call
returns `Forbidden`:

```rust,ignore
// Only return the wallet if the caller owns it.
#[firefly::post_authorize(result.owner == auth.subject())]
async fn get_wallet(&self, id: &str) -> Result<WalletView, AppError> { /* … */ }
```

Both macros read the caller from the ambient context in `firefly_security`:
`with_authentication_scope(auth, fut).await` runs a future with an
`Authentication` in scope, `current_authentication() -> Option<Authentication>`
reads it, and `check_access(&AccessRule) -> Result<Authentication, SecurityError>`
is the runtime check the macros expand to, over
`AccessRule::{Authenticated, Role, AnyRole, Authority, AnyAuthority}`. Because
`BearerLayer` scopes the authentication for the whole downstream call (on both the
anonymous and verified paths), these checks work on a service method that never
sees the `Request` — the macro reads from scope, not from a handler argument.

> **What it generates.** `#[pre_authorize]` emits a `check_access(&AccessRule)?`
> against the ambient context before your body, with an empty attribute defaulting
> to `AccessRule::Authenticated`. `#[post_authorize]` evaluates its boolean over
> `result` and `auth` after the body and converts `false` into a `Forbidden`
> error. Both rely on `SecurityError: From` for the function's error type, so
> denials travel the normal `Result` path.

## How it works: the `__rt` contract

A `proc-macro` crate cannot re-export runtime types, so macro-generated code
references every runtime type through the facade's hidden **`__rt` contract
path** — e.g. `::firefly::__rt::firefly_cqrs::Bus`. That is why Lumen, depending
only on `firefly` (plus the `axum`/`serde` it writes against anyway), compiles
whatever a macro expands to without listing the underlying `firefly-*` crates.
You never write `__rt` yourself. If you rename or shim the facade, pass
`#[firefly(crate = "my_firefly")]` to any macro to override the leading segment.

Rust has no reflective autowiring, so the declarative layer removes the
*mechanical* boilerplate, not the explicitness: Lumen still calls
`register(&bus)`, still calls `subscribe_project_wallet_event(broker)`, still
hands `WalletApi::routes(state)` to the web stack. The macros generate the
`impl`s, the routers, and the helper functions — the wiring you would otherwise
hand-write.

## The whole crate, declaratively

Read top to bottom, the macros tell Lumen's story:

```text
  money.rs        (no macros — a pure value object; the no-thiserror promise)
  domain.rs       #[derive(DomainEvent)] x3   #[derive(AggregateRoot)]
  ledger.rs       #[event_listener(topic = "wallets.events")]
  commands.rs     #[derive(Command)] x3   #[derive(Query)]
                  #[command_handler] x3   #[query_handler]
  transfer.rs     #[firefly::saga] + #[saga_step] x2
  compliance.rs   #[firefly::workflow] + #[workflow_step] x3
  tcc_transfer.rs #[firefly::tcc] + #[participant] x2
  security.rs     (JwtService / BearerLayer / FilterChain — runtime APIs)
  web.rs          #[rest_controller] + #[get] / #[post] x7
  housekeeping.rs #[scheduled(fixed_rate = "60s", initial_delay = "5s")]
```

Note what is *not* a macro: the security filter chain is built with a runtime
builder (`FilterChain::new().require(...)`), because its shape is data, not a
fixed declaration — and Lumen keeps it explicit so the control flow stays
visible. The saga, workflow, and TCC are now declarative macros
(`#[firefly::saga]`, `#[firefly::workflow]`, `#[firefly::tcc]`); only the filter
chain remains a runtime builder. Declarative where it collapses boilerplate,
explicit where the graph is the point: that balance is the whole design.

## Verifying the crate

Everything above compiles and is tested. From the workspace root:

```bash
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                       # 42 unit + 12 HTTP + 1 doctest
cargo test   -p firefly-sample-lumen --features streaming  # + 3 streaming tests
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
```

The HTTP tests drive the macro-generated `WalletApi::routes(...)` router
in-process through `tower::ServiceExt::oneshot` (no socket bound), proving the
generated routes, the CQRS handlers, validation (422), the not-found path (404),
the auth boundary (401), the transfer saga (happy + compensation), and the
projection convergence all work end to end — every prose listing in this book is
a slice of that running crate.

## What changed in Lumen

Nothing — this chapter is the retrospective, not a new feature. Re-read as a
catalogue, Lumen's macros are: three `#[derive(DomainEvent)]` + one
`#[derive(AggregateRoot)]` (`domain.rs`); one `#[event_listener]` (`ledger.rs`);
three `#[derive(Command)]` + one `#[derive(Query)]` + three `#[command_handler]`
+ one `#[query_handler]` (`commands.rs`); the declarative orchestration set —
`#[firefly::saga]` + `#[saga_step]` (`transfer.rs`), `#[firefly::workflow]` +
`#[workflow_step]` (`compliance.rs`), and `#[firefly::tcc]` + `#[participant]`
(`tcc_transfer.rs`); one `#[rest_controller]` with seven verb methods (`web.rs`);
and one `#[scheduled]` (`housekeeping.rs`). Each replaced a chunk of
hand-written wiring with a declaration next to the code — and all of it arrived
through one dependency and one prelude glob.

## Exercises

1. **Trace one macro end to end.** Pick `#[derive(Query)]` on `GetWallet`. Find
   where its generated `cache_ttl()` is read (the `QueryCache` middleware in
   `web.rs`) and the test that asserts it (`get_wallet_carries_cache_ttl` in
   `commands.rs`). Change the TTL to `"5s"` and re-run the tests.
2. **Add a verb.** Add a `#[get("/wallets/:id/events")]`-style read method to the
   `#[rest_controller]` impl (non-streaming: return the event list as JSON) and
   confirm `WalletApi::routes` picks it up with no other change.
3. **Add a scheduled task.** Write a second `#[scheduled(cron = "0 0 * * *")]`
   function in `housekeeping.rs`, register it in `build_scheduler`, and assert it
   appears in `scheduler.tasks()`.
4. **Count the wiring you didn't write.** For each macro in the catalogue, name
   the helper or impl it generated (`register_*`, `subscribe_*`, `schedule_*`,
   `routes`, `EVENT_TYPE`, `AGGREGATE_TYPE`). That list is the boilerplate the
   declarative layer wrote for you.

That is Lumen, complete and declarative. The appendices that follow are
reference: a [module index](./91-appendix-modules.md) and a
[glossary](./92-glossary.md).
