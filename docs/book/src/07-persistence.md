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
CRUD surface** that is the Rust analog of Spring Data R2DBC. This chapter covers
both, with a real streaming SQL repository.

> **Spring parity.** This is the Repository pattern as Spring Data popularized
> it, translated to idiomatic Rust. `Repository<T, K>` is the analog of
> `JpaRepository<T, ID>`; `ReactiveCrudRepository<T, ID>` is the analog of
> Spring Data R2DBC's `ReactiveCrudRepository<T, ID>`. You depend on the trait;
> the adapter supplies the SQL — exactly the JVM contract.

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

`Page<T>` is the canonical paged result, wire-identical to the
Java/.NET/Go/Python `Page<T>` so SDK clients deserialize it uniformly:

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

On top of the blocking contract, `firefly-data` adds a **reactive** surface —
the Spring Data R2DBC analog — built on `Mono` / `Flux` (the publishers from
[The Reactive Model](./05-reactive-model.md)). It is purely additive: nothing
about the existing `Repository` API changes.

| Spring Data R2DBC                 | firefly-data reactive                       |
|-----------------------------------|---------------------------------------------|
| `ReactiveCrudRepository<T, ID>`   | `ReactiveCrudRepository<T, ID>`             |
| `Flux<T> findAll()`               | `find_all() -> Flux<T>`                     |
| `Flux<T> findAllById(ids)`        | `find_all_by_id(ids) -> Flux<T>`            |
| `Mono<T> findById(id)`            | `find_by_id(id) -> Mono<T>`                 |
| `Mono<Boolean> existsById(id)`    | `exists_by_id(id) -> Mono<bool>`            |
| `Mono<T> save(e)`                 | `save(e) -> Mono<T>`                        |
| `Flux<T> saveAll(es)`             | `save_all(es) -> Flux<T>`                   |
| `Mono<Void> deleteById(id)`       | `delete_by_id(id) -> Mono<()>`              |
| `Mono<Void> deleteAll()`          | `delete_all() -> Mono<()>`                  |
| `Mono<Long> count()`              | `count() -> Mono<u64>`                       |
| `findAll(Specification, Pageable)`| `ReactiveSpecificationRepository`           |

A "no row" `find_by_id` maps to an **empty** `Mono` (Reactor's `Mono.empty()`),
exactly as Spring Data signals a missing `findById` — the reactive equivalent of
Lumen's `ReadModel::find` returning `None`.

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
with an optional `Pageable` window and **streams** the matches as a `Flux` — the
reactive analog of `findAll(Specification, Pageable)`, but with no intermediate
`Page<T>` envelope, so it plugs straight into an NDJSON / SSE endpoint with
backpressure.

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

Two adapter crates implement those ports so you code once and swap backends.

> **Spring parity.** This is hexagonal architecture / "ports & adapters" as
> Spring practices it: your service depends on the repository *port*; the
> *adapter* (the relational starter, the Mongo module) is chosen by
> configuration. Swapping Postgres for MySQL is the Rust analog of swapping a
> JDBC driver and dialect — a config change, not a code change.

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
> (`firefly = { version = "26.6", features = ["data-sqlx"] }`), re-exported as
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

## Transactions

`firefly-transactional` provides `with_tx(ctx, db, f)` over pluggable `Database`
/ `Transaction` ports, so a unit of work commits or rolls back as a whole. Bind
the ports to your driver and run your repository calls inside the closure. (The
write side of Lumen reaches durability differently — through the event store's
optimistic-concurrency `append`, covered in
[Event Sourcing](./11-event-sourcing.md) — but a read model upserted from many
events at once is a natural unit of work.)

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
