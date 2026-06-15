# Persistence & Reactive Repositories

Lumen has a wallet API that returns a `WalletView` — but where does that view
come from? At the end of [Your First HTTP API](./06-first-http-api.md) the
answer was "an in-memory map." This chapter gives Lumen a real persistence
vocabulary: the **repository pattern** as the seam between the query side and
storage, the in-memory baseline that keeps the teaching build infrastructure-free,
and the pluggable reactive-repository / SQL upgrade path that swaps that baseline
for Postgres, MySQL, SQLite, or MongoDB without touching a call site.

> **By the end of this chapter, Lumen will** have its query-side read store —
> the `ReadModel` — framed as a repository: a map of wallet id → `WalletView`
> that the `GetWallet` query reads from and the event projection (built in
> [EDA](./10-eda-messaging.md)) writes to. You will see the in-memory baseline
> that ships in `samples/lumen`, and exactly which `firefly-data` adapter you
> would drop in to make it durable — the book's recurring *swap the adapter*
> move.

`firefly-data` provides the framework's persistence vocabulary: a composable
`Filter` query DSL, a `Page<T>` paged-result envelope, a blocking
`Repository<T, K>` contract, and — built on `firefly-reactive` — a **reactive
CRUD surface** that returns `Mono` / `Flux` and streams rows lazily. This
chapter covers both, with a real streaming SQL repository.

> **Design note.** Firefly's data layer is the Repository pattern, expressed as
> idiomatic Rust: you depend on a trait — `Repository<T, K>` (blocking) or
> `ReactiveCrudRepository<T, ID>` (reactive) — and an adapter supplies the SQL.
> Depend on the port, swap the backend. If you have used a reactive-streams
> library before, the `Mono` / `Flux` surface will feel familiar.

## Lumen's read store, as a repository

Lumen splits its write model from its read model (the CQRS shape you build out
in [CQRS](./09-cqrs.md)). The write side is the event-sourced `Ledger`; the read
side is a flat, query-optimized `ReadModel` that `GET /api/v1/wallets/:id`
serves. In `samples/lumen` the read model is an in-memory map — small, exact,
and dependency-free, the right baseline for teaching:

```rust,ignore
// samples/lumen/src/ledger.rs — the CQRS query side.
use std::collections::HashMap;
use std::sync::Mutex;

use crate::domain::WalletView;

/// The in-memory read model: a map of wallet id → WalletView, upserted by the
/// projection and served by the GetWallet query. A real service would back this
/// with firefly's reactive repository over Postgres; an in-memory map keeps the
/// teaching baseline dependency-free.
#[derive(Debug, Default)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}

impl ReadModel {
    /// Upserts a projected view, replacing any previous row for the id.
    pub fn upsert(&self, view: WalletView) {
        self.rows
            .lock()
            .expect("read model lock")
            .insert(view.id.clone(), view);
    }

    /// Looks a projected view up by id.
    pub fn find(&self, id: &str) -> Option<WalletView> {
        self.rows.lock().expect("read model lock").get(id).cloned()
    }
}
```

Two things are deliberate. First, the surface is a **repository in miniature**:
`upsert(view)` and `find(id)` are the only operations the query side needs, so
those are the only operations it exposes. Second, the keys and values are the
plain domain types — a `WalletView` keyed by its `id` — so when you swap the map
for a database adapter, the *shape* the rest of Lumen sees does not move: a
`find` still returns `Option<WalletView>`, an `upsert` still takes one.

That is the whole point of treating the read store as a repository: the
`GetWallet` handler depends on "give me the view for this id," not on a
`HashMap`. Below is the production surface you would lower this onto — same
contract, a real database behind it.

## The `Page<T>` envelope

`Page<T>` is the canonical paged-result envelope with a stable, versioned JSON
shape — any client that honors that contract deserializes it uniformly, so
generated SDK clients consume it without per-service handling:

```rust,ignore
pub struct Page<T> {
    pub content: Vec<T>,
    pub number: usize,       // zero-based page index
    pub size: usize,
    pub total_elements: u64,
    pub total_pages: usize,  // derived
}
```

A *list wallets* endpoint — a natural Lumen extension — returns a
`Page<WalletView>` so a client can page through accounts without ever loading
the whole table.

## The `Filter` DSL

A `Filter` composes predicates, sorts, and a page window, and renders to a
parameterized `WHERE` clause via `to_sql()`:

```rust
use firefly_data::{Direction, Filter, Op, Predicate};
use serde_json::json;

let filter = Filter::default()
    .where_eq("owner", json!("alice"))
    .add(Predicate { field: "balance".into(), op: Op::Gte, value: json!(100_000) })
    .order_by("version", Direction::Desc)
    .paged(0, 20);

let (where_clause, args) = filter.to_sql();
// where_clause: a parameter-indexed " WHERE ..." fragment
// args:         the bound values, in order
assert!(where_clause.contains("WHERE"));
assert_eq!(args.len(), 2);
```

The operators (`Op`) cover `Eq`, `Ne`, `Lt`, `Lte`, `Gt`, `Gte`, `Like`,
`ILike`, `In`, and `IsNil` — the last skips an argument slot, so a predicate
list and its argument list stay aligned. A "rich wallets" query (`balance >=
100_000`, newest first) is exactly the filter above.

## The blocking `Repository` contract

`Repository<T, K>` is the object-safe `async_trait` port; `MemoryRepository`
implements it for tests, and you back it with your driver in production:

```rust,ignore
#[async_trait]
pub trait Repository<T, K>: Send + Sync {
    async fn find_by_id(&self, id: &K) -> Result<T, DataError>;
    async fn find(&self, filter: &Filter) -> Result<Page<T>, DataError>;
    // save / delete / count ...
}
```

This is the contract Lumen's `ReadModel` is a hand-rolled, two-method subset of.
Lower it onto `Repository<WalletView, String>` and `find` / `find_by_id` /
`save` come from the framework.

## The reactive CRUD surface

On top of the blocking contract, `firefly-data` adds a **reactive** CRUD surface
built on `Mono` / `Flux` (the publishers from
[The Reactive Model](./05-reactive-model.md)). It is purely additive: nothing
about the existing `Repository` API changes.

| Method                            | Returns                                     |
|-----------------------------------|---------------------------------------------|
| `find_all()`                      | `Flux<T>`                                   |
| `find_all_by_id(ids)`             | `Flux<T>`                                   |
| `find_by_id(id)`                  | `Mono<T>`                                   |
| `exists_by_id(id)`                | `Mono<bool>`                                |
| `save(e)`                         | `Mono<T>`                                   |
| `save_all(es)`                    | `Flux<T>`                                   |
| `delete_by_id(id)`                | `Mono<()>`                                  |
| `delete_all()`                    | `Mono<()>`                                  |
| `count()`                         | `Mono<u64>`                                 |
| `Specification` + `Pageable`      | `ReactiveSpecificationRepository`           |

A "no row" `find_by_id` resolves to an **empty** `Mono` — the reactive
equivalent of Lumen's `ReadModel::find` returning `None`.

### In-memory, for tests

`ReactiveMemoryRepository` is the reactive twin of `MemoryRepository`. Drive the
publishers with `block()` / `collect_list()`. Here it is holding wallet views —
the reactive version of Lumen's read store:

```rust
use firefly_data::{ReactiveCrudRepository, ReactiveMemoryRepository};

#[derive(Clone, PartialEq, Debug)]
struct WalletView { id: String, owner: String, balance: i64, version: i64 }

#[tokio::main]
async fn main() {
    let repo = ReactiveMemoryRepository::new(|w: &WalletView| w.id.clone());

    // save -> Mono<T>
    repo.save(WalletView { id: "wlt_1".into(), owner: "alice".into(), balance: 1000, version: 1 })
        .block().await.unwrap();

    // find_all -> Flux<T>, collected to a Vec
    let all = repo.find_all().collect_list().block().await.unwrap().unwrap();
    assert_eq!(all.len(), 1);

    // find_by_id miss -> empty Mono (Lumen's `ReadModel::find` returning None)
    assert_eq!(repo.find_by_id("ghost".into()).block().await.unwrap(), None);
    assert_eq!(repo.count().block().await.unwrap(), Some(1));
}
```

Swapping `ReadModel` for this repository is mechanical: `upsert` becomes `save`,
`find` becomes `find_by_id(...).block().await`, and the query handler keeps its
`Option<WalletView>` shape.

### Real SQL, streaming rows as a `Flux`

`PostgresReactiveRepository` is a production repository over `tokio-postgres`.
Reads drive the driver's `query_raw` **row stream**, so each row is decoded by a
`RowMapper` and emitted the moment it arrives over the wire — a million-row
table never lands fully in memory. Writes use a per-entity `inserter` closure
that renders a `T` to an upsert whose `RETURNING` projects the configured
columns. Backing Lumen's read model with it looks like this:

```rust,no_run
use std::sync::Arc;
use firefly_data::{PostgresReactiveRepository, ReactiveCrudRepository, TableConfig};
use firefly_kernel::FireflyError;
use tokio_postgres::{Row, types::ToSql, NoTls};

#[derive(Clone, PartialEq, Debug)]
struct WalletView { id: String, owner: String, balance: i64, version: i64 }

# async fn ex() -> Result<(), Box<dyn std::error::Error>> {
let (client, conn) =
    tokio_postgres::connect("postgres://localhost/lumen", NoTls).await?;
tokio::spawn(async move { let _ = conn.await; });
let client = Arc::new(client);

let repo: PostgresReactiveRepository<WalletView, String> = PostgresReactiveRepository::new(
    Arc::clone(&client),
    TableConfig::new("wallet_views", "id", ["id", "owner", "balance", "version"]),
    // RowMapper: decode a WalletView from each streamed row.
    |row: &Row| Ok(WalletView {
        id: row.try_get("id").map_err(|e| FireflyError::internal(e.to_string()))?,
        owner: row.try_get("owner").map_err(|e| FireflyError::internal(e.to_string()))?,
        balance: row.try_get("balance").map_err(|e| FireflyError::internal(e.to_string()))?,
        version: row.try_get("version").map_err(|e| FireflyError::internal(e.to_string()))?,
    }),
    // inserter: upsert RETURNING the projected columns (the projection's upsert).
    |w: &WalletView| (
        "INSERT INTO \"wallet_views\" (\"id\", \"owner\", \"balance\", \"version\") \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (\"id\") DO UPDATE SET \"owner\" = EXCLUDED.\"owner\", \
         \"balance\" = EXCLUDED.\"balance\", \"version\" = EXCLUDED.\"version\" \
         RETURNING \"id\", \"owner\", \"balance\", \"version\"".to_string(),
        vec![
            Box::new(w.id.clone()) as Box<dyn ToSql + Sync + Send>,
            Box::new(w.owner.clone()) as Box<dyn ToSql + Sync + Send>,
            Box::new(w.balance) as Box<dyn ToSql + Sync + Send>,
            Box::new(w.version) as Box<dyn ToSql + Sync + Send>,
        ],
    ),
);

// Rows stream lazily out of find_all() as a Flux — a *list wallets* endpoint.
let all = repo.find_all().collect_list().block().await?.unwrap();
# Ok(())
# }
```

Use `stream_query(sql, params)` for custom derived queries: any `SELECT`
projecting the configured columns is streamed row-by-row through the same
`RowMapper`. This `Flux` plugs directly into a `NdJson` / `Sse` endpoint, so a
database read streams to the client end-to-end with backpressure — no
collect-then-emit step anywhere in the path. (That is the same `Flux → NdJson`
seam Lumen's events endpoint uses, only sourced from rows instead of an event
store.)

### Reactive specification / paging

`ReactiveSpecificationRepository` runs a composable `Specification` predicate
with an optional `Pageable` window and **streams** the matches as a `Flux`, with
no intermediate `Page<T>` envelope — so it plugs straight into an NDJSON / SSE
endpoint with backpressure.

### Sorting & paging — the `ReactiveSortingRepository` (free)

firefly-data's `ReactiveSortingRepository<T, ID>` adds whole-collection sorting
and paging — `find_all_sorted(RequestSort) -> Flux<T>` and
`find_all_paged(Pageable) -> Flux<T>` — and you write **no** code for it: it is a
blanket `impl` over any repository that is both a `ReactiveCrudRepository` and a
`ReactiveSpecificationRepository`. The sort/page is run as a match-all
`Specification`, so every `SqlxReactiveRepository` and `ReactiveMemoryRepository`
acquires it automatically.

```rust
use firefly_data::{
    Pageable, ReactiveCrudRepository, ReactiveMemoryRepository, ReactiveSortingRepository,
    RequestSort,
};

#[derive(Clone, PartialEq, Debug, serde::Serialize)]
struct WalletView { id: String, owner: String, balance: i64 }

#[tokio::main]
async fn main() {
    let repo = ReactiveMemoryRepository::new(|w: &WalletView| w.id.clone());
    for (id, owner) in [("w1", "carol"), ("w2", "alice"), ("w3", "bob")] {
        repo.save(WalletView { id: id.into(), owner: owner.into(), balance: 0 })
            .block().await.unwrap();
    }

    // find_all(Sort) — ordered by owner ascending, streamed as a Flux.
    let sorted = repo
        .find_all_sorted(RequestSort::by(["owner"]))
        .collect_list().block().await.unwrap().unwrap();
    assert_eq!(sorted[0].owner, "alice");

    // find_all(Pageable) — page 1 (1-based), size 2, sorted; a Flux window.
    let page = repo
        .find_all_paged(Pageable::of(1, 2, RequestSort::by(["owner"])).unwrap())
        .collect_list().block().await.unwrap().unwrap();
    assert_eq!(page.len(), 2);
}
```

> **Note.** `find_all_paged` streams the page as a `Flux` window rather than
> buffering a `Page<T>` envelope — reach for `Page<T>` + a count query when you
> actually need totals.

## Pluggable databases — a new DB is a new adapter

`firefly-data` is the **ports** crate: it owns no driver and implies no SQL
engine. The `Filter` DSL, the composable `Specification`, the repository traits,
and the auditing / soft-delete policies are all storage-agnostic. The lowering
surface is what makes this hexagonal:

- a **`SqlDialect`** trait with three shipped impls — `PostgresDialect`,
  `MySqlDialect`, `SqliteDialect` — so `Filter::to_sql_with(&dialect)` and
  `Specification::to_sql_with(&dialect)` render the *same* query tree per
  backend, getting placeholder style (`$1` vs `?`), identifier quoting (`"id"`
  vs `` `id` ``), `IN`-list shape, and case-insensitive `LIKE` right for each.
  (`Filter::to_sql` / `Specification::to_sql` stay the PostgreSQL default.)
- a **`Specification::to_mongo()`** / `Filter::to_mongo()` that lowers the same
  tree to a MongoDB `$`-operator filter document.

Two adapter *crates* — `firefly-data-sqlx` (covering the three relational
backends) and `firefly-data-mongodb` — implement those ports so you code once
and swap backends.

> **Design note.** This is hexagonal architecture — ports & adapters. Your
> service depends on the repository *port*; the *adapter* (the relational crate,
> the Mongo crate) is chosen at wiring time. Swapping Postgres for MySQL is a
> pool change, not a code change — the call sites never move.

### Relational — `firefly-data-sqlx` (Postgres / MySQL / SQLite)

`SqlxReactiveRepository` (and the blocking `SqlxRepository`) serve all three
relational backends from one codebase. A `Db` enum tags a `PgPool` /
`MySqlPool` / `SqlitePool` with its `Backend`, and the repository picks the
matching `SqlDialect` at runtime — so "new relational DB = new pool", not "new
adapter". `UPSERT` is dialect-aware (`ON CONFLICT … DO UPDATE` for
Postgres/SQLite, `ON DUPLICATE KEY UPDATE` for MySQL), reads stream off sqlx's
row stream into a `Flux`, and an optional `Auditor` / `SoftDeletePolicy`
auto-stamps and hides rows on every write/read.

SQLite-in-memory is the **no-infrastructure default** — the same role the
in-memory map plays in `samples/lumen`, but exercising the real adapter:

```rust
use firefly_data::{ReactiveCrudRepository, TableConfig};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone, PartialEq)]
struct WalletView { id: String, owner: String, balance: i64 }

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
sqlx::query(r#"CREATE TABLE "wallet_views" ("id" TEXT PRIMARY KEY, "owner" TEXT NOT NULL, "balance" BIGINT NOT NULL)"#)
    .execute(&pool).await.unwrap();

let repo: SqlxReactiveRepository<WalletView, String> = SqlxReactiveRepository::new(
    Db::Sqlite(pool),
    TableConfig::new("wallet_views", "id", ["id", "owner", "balance"]),
    // RowMapper: decode by column name — backend-agnostic via AnyRow.
    |row: &AnyRow| Ok::<_, FireflyError>(WalletView {
        id: row.get_str("id")?,
        owner: row.get_str("owner")?,
        balance: row.get_i64("balance")?,
    }),
    // RowWriter: the entity's (column, value) pairs.
    |w: &WalletView| vec![
        ColumnValue::new("id", w.id.clone()),
        ColumnValue::new("owner", w.owner.clone()),
        ColumnValue::new("balance", w.balance),
    ],
);
repo.save(WalletView { id: "wlt_1".into(), owner: "alice".into(), balance: 1000 })
    .block().await.unwrap();
# });
```

Switching to Postgres or MySQL is `Db::Postgres(pg_pool)` / `Db::MySql(my_pool)`
— the repository call sites do not change. That is the upgrade path the
`samples/lumen` comment promises: the in-memory `ReadModel` becomes a
`SqlxReactiveRepository<WalletView, String>`, and the `GetWallet` handler is
none the wiser.

The constructor takes a `Db`, a `TableConfig`, a `RowMapper` (reads), and a
`RowWriter` (writes); three chainable builders add cross-cutting behaviour, each
returning a fresh repository:

- `.with_auditor(Auditor)` — stamps `created_at` / `updated_at` /
  `created_by` / `updated_by` on every write (insert vs update is decided by
  whether the row already exists): automatic auditing.
- `.with_soft_delete(SoftDeletePolicy)` — hides soft-deleted rows from every
  read and turns `delete_by_id` into a `deleted_at` stamp instead of a physical
  `DELETE`: logical (soft) delete.
- `.with_version_column("version")` — turns on optimistic locking
  (next section).

### Building the pool from config — `auto_configure`

In the examples above the pool is constructed by hand. In a real service the
connection settings live in configuration, and Firefly turns them into a live
pool — plus a registered transaction manager — in a single awaited call at
startup. There is no dependency-injection container in the loop: you load the
config, bind a plain `serde` struct, and `await` one function.

`DataSourceProperties` is that struct, bound from the `firefly.datasource.*`
config tree:

```rust,ignore
use firefly_data_sqlx::DataSourceProperties;

// Bound from `firefly.datasource.*` (e.g. an application.yaml / env overrides).
pub struct DataSourceProperties {
    pub url: String,                  // scheme picks the backend (see below)
    pub max_connections: u32,         // `0` leaves the driver default
    pub min_connections: u32,         // `0` leaves the driver default
    pub acquire_timeout_ms: u64,      // `0` leaves the driver default
    pub idle_timeout_ms: u64,         // `0` leaves the driver default
    pub max_lifetime_ms: u64,         // `0` leaves the driver default
}
```

The **URL scheme selects the backend** (each behind its cargo feature):
`postgres://` / `postgresql://` → PostgreSQL, `mysql://` → MySQL, `sqlite:` →
SQLite. So a config change from `postgres://…` to `mysql://…` moves the whole
service to MySQL with no code edit — the pluggable-database promise, driven from
configuration.

Three entry points build the `Db`:

- `Db::connect(url).await -> Result<Db, FireflyError>` — a pool from a URL,
  using driver defaults.
- `Db::connect_with(&props).await` — a pool honouring the full
  `DataSourceProperties` (sizes, timeouts, lifetimes).
- `data_sqlx::auto_configure(&props).await` — the **one-call startup path**: it
  builds the pool *and* registers a `SqlxTransactionManager`, so
  `#[firefly::transactional]` resolves its manager with no manual wiring. The
  returned `Db` then builds your typed repositories.

The shape of a boot sequence is: load config → bind `DataSourceProperties` →
`await auto_configure` once → build repositories from the returned `Db`.

```rust,ignore
use firefly_data::TableConfig;
use firefly_data_sqlx::{auto_configure, AnyRow, ColumnValue, DataSourceProperties, SqlxReactiveRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone, PartialEq)]
struct WalletView { id: String, owner: String, balance: i64 }

# async fn boot(props: DataSourceProperties) -> Result<(), Box<dyn std::error::Error>> {
// One awaited call: builds the pool AND registers the SqlxTransactionManager.
let db = auto_configure(&props).await?;

// The returned Db builds typed repositories — no DI container involved.
let wallets: SqlxReactiveRepository<WalletView, String> = SqlxReactiveRepository::new(
    db.clone(),
    TableConfig::new("wallet_views", "id", ["id", "owner", "balance"]),
    |row: &AnyRow| Ok::<_, FireflyError>(WalletView {
        id: row.get_str("id")?,
        owner: row.get_str("owner")?,
        balance: row.get_i64("balance")?,
    }),
    |w: &WalletView| vec![
        ColumnValue::new("id", w.id.clone()),
        ColumnValue::new("owner", w.owner.clone()),
        ColumnValue::new("balance", w.balance),
    ],
);
// Because auto_configure registered the manager, a `#[firefly::transactional]`
// fn that writes through `wallets` is now atomic with no further wiring.
# Ok(())
# }
```

> **Design note.** Configuration drives the runtime, not a container. A plain
> `serde` struct is bound from `firefly.datasource.*`, and a single awaited
> `auto_configure` builds the pool and registers the transaction manager — the
> wiring is explicit, compiler-checked, and visible in one place rather than
> assembled by reflection at runtime.

### Optimistic locking — `with_version_column`

`with_version_column("version")` makes a `save` a **version-guarded** conditional
upsert: every write bumps the version column and guards the conflict-update on
the version the entity was loaded with (`WHERE version = <loaded>`). If a
concurrent writer moved the stored version on, the guarded update matches zero
rows and the save fails with `DataError::OptimisticLock` rather than silently
overwriting the other change. The entity's `RowWriter` must emit the version
column carrying the loaded value. (Conflict detection is enforced on Postgres and
SQLite; on MySQL the version is bumped but the guard is not applied.)

```rust,ignore
use firefly_data::{DataError, Repository, TableConfig};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone)]
struct Account { id: String, balance: i64, version: i64 }

# async fn ex(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
let repo: SqlxRepository<Account, String> = SqlxRepository::new(
    Db::Postgres(pool),
    TableConfig::new("accounts", "id", ["id", "balance", "version"]),
    |row: &AnyRow| Ok::<_, FireflyError>(Account {
        id: row.get_str("id")?,
        balance: row.get_i64("balance")?,
        version: row.get_i64("version")?,
    }),
    |a: &Account| vec![
        ColumnValue::new("id", a.id.clone()),
        ColumnValue::new("balance", a.balance),
        // The loaded version — the conditional upsert guards on it.
        ColumnValue::new("version", a.version),
    ],
)
.with_version_column("version");

// Two callers loaded the same Account at version 1. The first save wins
// (the row is now version 2); the second save's guard (WHERE version = 1)
// matches nothing, so it fails with OptimisticLock — the caller reloads + retries.
let stale = repo.save(Account { id: "acc_1".into(), balance: 50, version: 1 }).await;
assert!(matches!(stale, Err(DataError::OptimisticLock)));
# Ok(())
# }
```

> **Note.** A stale write fails with `DataError::OptimisticLock` rather than
> silently overwriting a concurrent change; the caller reloads and retries.
> Lumen's *write* side reaches the same "lost-update prevention" guarantee
> differently — through the event store's optimistic-concurrency `append` (see
> [Event Sourcing](./11-event-sourcing.md)) — but a relational `Account` /
> `Order` repository uses a version column.

### Declarative derived queries — `#[firefly::repository]`

Beyond CRUD, the `#[firefly::repository]` macro derives a query straight from a
*method name*: `find_by_status(&str)` becomes `WHERE status = ?`. Apply it to an
`impl` block of **typed stub methods** named with the framework's grammar
(`find_by_…`, `count_by_…`, `exists_by_…`, `delete_by_…`); the macro discards the
placeholder body and generates a real one that marshals the arguments and
delegates to the tested runtime engine. The return type selects the operation:

| Return shape                          | Generated call        |
|---------------------------------------|-----------------------|
| `Result<Vec<T>, DataError>`           | `find_by_derived`     |
| `Result<Option<T>, DataError>`        | `find_by_derived` (first) |
| `Result<i64, DataError>`              | `count_by_derived`    |
| `Result<bool, DataError>`             | `exists_by_derived`   |
| `Result<u64, DataError>`              | `delete_by_derived`   |

Each impl-block type exposes the backing repository via `self.repository()`
(override with `#[repository(repo = "…")]`), returning a
`SqlxReactiveRepository<Entity, Id>`:

```rust,ignore
use firefly_data::DataError;
use firefly_data_sqlx::SqlxReactiveRepository;

struct Account { /* … */ }

struct AccountRepo {
    repo: SqlxReactiveRepository<Account, String>,
}

impl AccountRepo {
    // The accessor the macro calls (default name `repository`).
    fn repository(&self) -> &SqlxReactiveRepository<Account, String> {
        &self.repo
    }
}

#[firefly::repository]
impl AccountRepo {
    async fn find_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }
    async fn find_by_owner_and_status(&self, owner: &str, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }
    async fn count_by_owner(&self, owner: &str) -> Result<i64, DataError> { unimplemented!() }
    async fn exists_by_email(&self, email: &str) -> Result<bool, DataError> { unimplemented!() }
    async fn delete_by_status(&self, status: &str) -> Result<u64, DataError> { unimplemented!() }
}
```

#### Paged derived queries — a trailing `Pageable`

A `find_by_…` method whose **last argument is a `Pageable`** and which returns
`Result<Vec<T>, DataError>` is a *paged* derived query: the pageable's sort and
window are appended to the generated `WHERE`, and the runtime backs it with
`SqlxReactiveRepository::find_by_derived_paged(method_name, &args, &Pageable)`.
Build the page with `Pageable::of(page, size, sort)` — `page` is **1-based** —
and `RequestSort::of([Order::desc("id")])` for the ordering:

```rust,ignore
use firefly_data::{DataError, Order, Pageable, RequestSort};

#[firefly::repository]
impl AccountRepo {
    // The trailing Pageable makes this a paged query: WHERE owner = ?,
    // then the pageable's ORDER BY + LIMIT/OFFSET window.
    async fn find_by_owner(&self, owner: &str, page: Pageable) -> Result<Vec<Account>, DataError> {
        unimplemented!()
    }
}

# async fn ex(accounts: &AccountRepo) -> Result<(), DataError> {
let page = Pageable::of(1, 20, RequestSort::of([Order::desc("id")])).unwrap();
let rows = accounts.find_by_owner("alice", page).await?;
# Ok(())
# }
```

#### Custom queries — `#[query(...)]`

When the method-name grammar can't express the query you need, annotate the stub
with `#[query(...)]` and write the SQL yourself. A `:name` placeholder binds to
the argument named `name`, and the **return type selects the operation** exactly
as for derived methods — `Vec<T>` / `Option<T>` for a list, `i64` for a count,
`bool` for an exists, and `u64` for a *modifying* statement (`INSERT` / `UPDATE`
/ `DELETE`, returning the affected-row count):

```rust,ignore
use firefly_data::DataError;

#[firefly::repository]
impl AccountRepo {
    // Native SQL; :status binds to the `status` argument.
    #[query("SELECT id, owner FROM accounts WHERE status = :status ORDER BY id DESC")]
    async fn list_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> {
        unimplemented!()
    }

    // i64 return -> a count query.
    #[query("SELECT COUNT(*) FROM accounts WHERE owner = :owner")]
    async fn tally(&self, owner: &str) -> Result<i64, DataError> { unimplemented!() }

    // u64 return -> a modifying statement; the value is the affected-row count.
    #[query("UPDATE accounts SET status = :to WHERE status = :from")]
    async fn retire(&self, from: &str, to: &str) -> Result<u64, DataError> { unimplemented!() }
}
```

`#[query("…")]` is shorthand for `#[query(sql = "…")]` (native SQL). For a
portable, entity-oriented query use the JPQL-like form
`#[query(jpql = "…", entity = "Account")]`, whose `FROM <Entity>` is transpiled
to the configured table so the same string runs on Postgres, MySQL, or SQLite.

Under the hood the runtime engine does two things. `QueryMethodParser` parses the
method name — prefix (`find` / `count` / `exists` / `delete`), `By`, then a chain
of `And` / `Or` property conditions — into a query the active `SqlDialect` lowers
to a parameterized statement, so the same `find_by_owner_and_status` runs on
Postgres, MySQL, or SQLite; a trailing `Pageable` routes to
`find_by_derived_paged`. The `#[query(...)]` attribute, in turn, lowers to the
repository's `query_list` / `query_count` / `query_exists` / `query_execute`
helpers — list, count, exists, and modifying ops respectively — binding the
`:name` placeholders and (for the JPQL form) transpiling `FROM <Entity>` to the
table.

> **Tip.** Reach for the method-name grammar for simple predicates and a trailing
> `Pageable` for paged reads; use `#[query(...)]` for anything the name grammar
> can't express. A relational `Account` / `Order` service writes these; Lumen's
> event-sourced read side stays a hand-rolled in-memory `ReadModel` instead.

### Document — `firefly-data-mongodb` (MongoDB)

`MongoRepository<T, ID>` puts a MongoDB collection behind the **same**
`ReactiveCrudRepository` + `ReactiveSpecificationRepository` traits, lowering a
`Specification` via `Specification::to_mongo()` exactly as the relational
adapters lower it via `to_sql`. A `BaseDocument` mixin (embedded with
`#[serde(flatten)]`) carries the audit stamps and soft-delete column, and
`with_soft_delete(policy)` makes every read inject a `{"<column>": null}` guard
while `delete_by_id` becomes a logical delete. Reads stream lazily off the
driver cursor as a `Flux`.

```rust,no_run
use firefly_data::{ReactiveCrudRepository, Specification};
use firefly_data_mongodb::{BaseDocument, MongoRepository};
use mongodb::bson::{Bson, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct WalletDocument {
    #[serde(rename = "_id")] id: String,
    owner: String,
    balance: i64,
    #[serde(flatten)] base: BaseDocument,
}

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
let collection = client.database("lumen").collection::<Document>("wallet_views");
let repo: MongoRepository<WalletDocument, String> =
    MongoRepository::new(collection, |w: &WalletDocument| Bson::String(w.id.clone()));

repo.save(WalletDocument {
    id: "wlt_1".into(), owner: "alice".into(), balance: 1000, base: BaseDocument::new(),
}).block().await?;
# Ok(())
# }
```

Because all four backends sit behind the same ports, a service that codes
against `Repository` / `ReactiveCrudRepository` / `Specification` can move from
Postgres to MySQL, SQLite, or MongoDB by swapping the adapter constructor — and
adding a *new* database is "write a `firefly-data-<tech>` crate that implements
the ports", not "rewrite the data layer." Both adapters are tested against real
Postgres, MySQL, SQLite, and MongoDB.

> **One-dependency note.** Lumen pulls none of these adapters in its default
> build — they are opt-in cargo features on the `firefly` facade
> (`firefly = { version = "26.7", features = ["data-sqlx"] }`), re-exported as
> `firefly::data_sqlx` / `firefly::data_mongodb`. The teaching build stays lean;
> the production build adds exactly the driver it needs.

## Schema migrations

`firefly-migrations` is a forward-only SQL migration runner. Migration files are
named `V{version}__{description}.sql` (e.g. `V001__init.sql`); each runs once, in
version order, inside a transaction. The runner works against any store behind
the small synchronous `Database` port:

```rust,ignore
use firefly_migrations::{run, DirSource};

let src = DirSource { dir: "migrations".into() };
run(&mut db, &src)?;                 // applies pending migrations in order
let status = firefly_migrations::inspect(&mut db, &src)?; // applied + pending
```

The [CLI](./19-cli.md) wraps this: `firefly db init`, `firefly db migrate -m
"create wallet_views"`, `firefly db upgrade --url sqlite://lumen.db`, and
`firefly db status`. A `V001__wallet_views.sql` creating the read-model table is
all the schema Lumen's durable read side needs.

## Transactions — `#[firefly::transactional]`

`firefly-transactional` brings declarative transactions to Rust: annotate an
`async fn` that returns `Result<_, E>` (where
`E: From<firefly::transactional::TxError>`) with `#[firefly::transactional]` and
the body runs inside a transaction — **commit on `Ok`, rollback on `Err`**.

```rust,ignore
use firefly::transactional::TxError;
use firefly_data_sqlx::SqlxReactiveRepository;

#[derive(Debug)]
enum TransferError { /* … */ Tx(TxError) }
impl From<TxError> for TransferError { fn from(e: TxError) -> Self { TransferError::Tx(e) } }

struct Accounts { repo: SqlxReactiveRepository<Account, String> }
struct Ledger   { repo: SqlxReactiveRepository<Entry, String> }

#[derive(Debug, Clone)] struct Account { id: String, balance: i64 }
#[derive(Debug, Clone)] struct Entry   { id: String, account: String, delta: i64 }

#[firefly::transactional(
    propagation = "requires_new",
    isolation = "serializable",
    read_only,
    timeout_ms = 5000,
)]
async fn record(accounts: &Accounts, ledger: &Ledger) -> Result<(), TransferError> {
    // … repository writes here join this transaction automatically …
    Ok(())
}
```

The attributes are `propagation` (`required` / `requires_new` / `nested` /
`supports` / `not_supported` / `mandatory` / `never`), `isolation`
(`read_committed` / `repeatable_read` / `serializable` / …), `read_only`, and
`timeout_ms`.

**Ambient enlistment** is what makes this seamless. The manager opens a sqlx
transaction and stows it in a task-local stack; while that scope is active,
every `SqlxReactiveRepository` / `SqlxRepository` write *inside the fn* routes
onto the active transaction instead of a fresh pool connection — so a plain
sequence of `repo.save(...).await?` calls is atomic with **no change to the
repository code**. An atomic two-repository money transfer:

```rust,ignore
use firefly::transactional::TxError;

#[firefly::transactional]   // defaults: REQUIRED, datasource isolation, read-write
async fn transfer(
    accounts: &SqlxReactiveRepository<Account, String>,
    ledger: &SqlxReactiveRepository<Entry, String>,
    from: Account,
    to: Account,
    amount: i64,
) -> Result<(), TxError> {
    // All four writes enlist in the same ambient transaction. If any await
    // returns Err, the whole unit of work rolls back; otherwise it commits.
    accounts.save(Account { balance: from.balance - amount, ..from.clone() })
        .into_future().await?;
    accounts.save(Account { balance: to.balance + amount, ..to.clone() })
        .into_future().await?;
    ledger.save(Entry { id: "e1".into(), account: from.id, delta: -amount })
        .into_future().await?;
    ledger.save(Entry { id: "e2".into(), account: to.id, delta: amount })
        .into_future().await?;
    Ok(())
}
```

For programmatic control there is `firefly::transactional::transactional(opts,
f)` and `transactional_on(&manager, opts, f)` for an explicit manager, with
`TxOptions`, `Propagation`, and `Isolation` builders. The sqlx adapter —
`SqlxTransactionManager`, registered once at startup (the `auto_configure` path
below does this for you) — supplies the real behaviour: full propagation
(`REQUIRED` / `REQUIRES_NEW` / `NESTED` / `SUPPORTS` / `NOT_SUPPORTED` /
`MANDATORY` / `NEVER`), isolation, read-only, a statement timeout, and
`SAVEPOINT`-based `NESTED` nesting.

> **Note.** The killer feature is **ambient enlistment**: while a transactional
> scope is active, every repository write inside the fn joins the active
> transaction automatically — a plain sequence of `repo.save(...).await?` calls
> is atomic with no change to the repository code, carried on a task-local rather
> than a thread-bound connection. (Lumen's write side reaches durability
> differently — through the event store's optimistic-concurrency `append`,
> covered in [Event Sourcing](./11-event-sourcing.md) — but a relational
> `Account` / `Order` service spanning two repositories is exactly what
> `#[transactional]` is for.)

## What changed in Lumen

Lumen now has a clear persistence story, even though its teaching build stays
infrastructure-free:

- **The read store is a repository.** `ReadModel` (in `samples/lumen/src/ledger.rs`)
  is a `Mutex<HashMap<String, WalletView>>` exposing exactly `upsert(view)` and
  `find(id)` — the two operations the query side needs, keyed by the
  `WalletView`'s own id. The `GetWallet` handler depends on the *contract*, not
  the map.
- **The baseline is in-memory by choice.** The map keeps the dependency footprint
  at one Firefly crate; the comment in the source names the upgrade explicitly —
  "a real service would back this with firefly's reactive repository over
  Postgres."
- **The upgrade is an adapter swap.** Lowering `ReadModel` onto
  `ReactiveCrudRepository<WalletView, String>` — `SqlxReactiveRepository` for
  Postgres/MySQL/SQLite, `MongoRepository` for Mongo — turns `upsert` into `save`
  and `find` into `find_by_id`, with the query handler's `Option<WalletView>`
  shape unchanged. A new database is a new pool (relational) or a new adapter
  crate, never a rewrite.

## Exercises

1. **Re-skin `ReadModel` as a trait.** Define
   `trait WalletViews { fn upsert(&self, v: WalletView); fn find(&self, id: &str) -> Option<WalletView>; }`,
   implement it for the in-memory `ReadModel`, and have the `GetWallet` handler
   take `&dyn WalletViews`. Confirm the rest of Lumen still compiles — proof the
   query side depends on the contract, not the map.

2. **Back the read model with SQLite.** Using the `SqlxReactiveRepository`
   listing above, create a `wallet_views` table in `sqlite::memory:`, `save` two
   views, and `find_all().collect_list()` them. Assert both come back. This is
   the production read store, exercised end to end against the real adapter.

3. **Page the wallets.** Build a `Filter` that selects wallets with
   `balance >= 100_000` ordered by `version` descending, page `(0, 20)`, and
   print its `to_sql()`. Then describe (one sentence each) how the same filter
   would render under `MySqlDialect` and `SqliteDialect`.

4. **Trace the swap.** List the exact lines in `samples/lumen/src/ledger.rs` that
   would change if `ReadModel` became a `SqlxReactiveRepository<WalletView,
   String>`, and which lines in `samples/lumen/src/commands.rs` (the `GetWallet`
   handler) would *not* — confirming the adapter boundary holds.

With persistence framed, the next chapter models the business itself — the
`Money` value object and the `Wallet` aggregate. See
[Domain-Driven Design](./08-domain-driven-design.md).
