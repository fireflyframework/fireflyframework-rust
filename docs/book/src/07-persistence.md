# Persistence & Reactive Repositories

Lumen already has a wallet API that returns a `WalletView` — but where does that
view come from? At the end of [Your First HTTP API](./06-first-http-api.md) the
honest answer was "an in-memory map." This chapter gives Lumen a real
persistence vocabulary and shows the exact upgrade path from that teaching map to
a durable database — without touching a single call site.

The throughline is one move the book repeats: *depend on the contract, swap the
backend.* Lumen's read store is already framed as a repository; here you learn
the framework contract it is a miniature of, the reactive CRUD surface that
streams rows lazily, the relational and document adapters that implement that
surface over Postgres / MySQL / SQLite / MongoDB, and the transaction boundary
that makes a multi-write change atomic. The teaching build stays
infrastructure-free the whole way — every durable piece is exercised against an
in-memory SQLite or in-process double, so nothing here needs a running server.

By the end of this chapter you will:

- Explain the **repository pattern** as the seam between the query side and
  storage, and recognise Lumen's `ReadModel` as a hand-rolled repository in
  miniature.
- Compose a `Filter` query, render it to parameterized SQL, and read a `Page<T>`
  paged-result envelope.
- Drive the **reactive CRUD surface** — `Mono` / `Flux` repositories — against an
  in-memory double and a real streaming SQLite/Postgres adapter.
- Declare a repository the Spring Data way with `#[derive(Entity)]` +
  `#[derive(SqlxRepository)]`, and add derived and custom queries with
  `#[firefly::repository]`.
- Turn on **optimistic locking** and build a pool from configuration with one
  awaited `auto_configure` call.
- Make a multi-write change atomic with `#[firefly::transactional]` and its
  ambient enlistment.

## Concepts you will meet

Before the first listing, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — repository.** A *repository* is an object that hides how
> entities are stored behind a small, intention-revealing set of operations —
> `find_by_id`, `save`, `delete`. Callers depend on the repository *interface*,
> not on SQL or a `HashMap`. This is exactly Spring Data's `Repository` /
> `CrudRepository`.

> **Note** **Key term — port and adapter.** A *port* is an interface your code
> depends on; an *adapter* is a concrete implementation chosen at wiring time.
> `firefly-data` owns the ports (the repository traits, the query DSL);
> `firefly-data-sqlx` and `firefly-data-mongodb` are adapters. Swapping the
> adapter swaps the database with no change to call sites — this is *hexagonal
> architecture* (ports & adapters).

> **Note** **Key term — `Mono` / `Flux`.** These are the reactive *publishers*
> from [The Reactive Model](./05-reactive-model.md): a `Mono<T>` resolves to at
> most one value, a `Flux<T>` to a lazy, backpressured stream of many. The
> reactive repository returns them so a database read can stream row-by-row to
> the client. If you have used a reactive-streams library (Project Reactor,
> RxJava), they are the same `Mono` / `Flux`.

> **Note** **Key term — optimistic locking.** A concurrency strategy where each
> row carries a *version* number; a write succeeds only if the version it loaded
> still matches the stored one, otherwise it is rejected rather than silently
> overwriting a concurrent change. This is Spring Data's `@Version`.

> **Design note.** Firefly's data layer is the Repository pattern expressed as
> idiomatic Rust: you depend on a trait — `Repository<T, K>` (blocking) or
> `ReactiveCrudRepository<T, ID>` (reactive) — and an adapter supplies the SQL.
> Depend on the port, swap the backend. `firefly-data` itself owns no driver and
> implies no SQL engine; that is what makes the swap mechanical.

## Step 1 — See Lumen's read store as a repository

Lumen splits its write model from its read model — the
Command/Query Responsibility Segregation (CQRS) shape you build out in
[CQRS](./09-cqrs.md). The write side is the event-sourced `Ledger`; the read
side is a flat, query-optimized `ReadModel` that `GET /api/v1/wallets/:id`
serves. In `samples/lumen` the read model is an in-memory map — small, exact, and
dependency-free, the right baseline for teaching.

Open `samples/lumen/src/ledger.rs` and read the read-model type:

```rust,ignore
// samples/lumen/src/ledger.rs — the CQRS query side.
use std::collections::HashMap;
use std::sync::Mutex;

use firefly::prelude::*;
use crate::domain::WalletView;

/// The in-memory read model: a map of wallet id → WalletView, upserted by the
/// projection and served by the GetWallet query. It carries
/// `#[derive(Repository)]` (Spring's `@Repository`), so `container.scan()`
/// registers it as a data-access singleton — autowired as `Arc<ReadModel>` into
/// the handler and projection beans. A real service would back this with
/// firefly's reactive repository over Postgres; an in-memory map keeps the
/// teaching baseline dependency-free.
#[derive(Debug, Default, Repository)]
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

What just happened: two design choices are deliberate.

- The surface is a **repository in miniature**. `upsert(view)` and `find(id)` are
  the only operations the query side needs, so those are the only operations it
  exposes — a hand-rolled, two-method subset of the framework repository contract
  you meet in Step 4.
- The keys and values are plain domain types — a `WalletView` keyed by its `id`.
  So when you later swap the map for a database adapter, the *shape* the rest of
  Lumen sees does not move: `find` still returns `Option<WalletView>`, `upsert`
  still takes one.

> **Note** **Key term — `#[derive(Repository)]`.** This derive marks a type as a
> data-access bean — Spring's `@Repository`. The component scan registers it as a
> singleton, so it is autowired (as `Arc<ReadModel>`) into the query handler and
> the projection that feeds it. The derive is about *wiring* the object into the
> container; the storage behind it is whatever the struct holds — here a
> `Mutex<HashMap<…>>`.

Why it matters: the `GetWallet` handler depends on "give me the view for this
id," not on a `HashMap`. That is the whole point of treating the read store as a
repository — and the reason the rest of this chapter can replace the map with a
real database without the handler noticing.

> **Tip** **Checkpoint.** From a checkout of the framework, run
> `cargo test -p lumen --lib ledger` and watch the read-model round-trip tests
> pass. You have confirmed the in-memory baseline works before swapping anything
> underneath it.

## Step 2 — Compose a query with the `Filter` DSL

Before lowering the read store onto a real database, you need a way to *ask* for
rows — a query value the adapters can render to SQL. `firefly-data` provides one:
the `Filter` DSL.

> **Note** **Key term — `Filter` DSL.** A `Filter` is a composable value that
> bundles a list of predicates (field, operator, value), zero or more sort
> orders, and a page window. It renders to a parameterized `WHERE` clause via
> `to_sql()` — never string-interpolated, so values bind as `$1`, `$2`, … and
> SQL injection is structurally impossible.

Build a "rich wallets" query — `balance >= 100_000`, newest first, first page of
20:

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

What just happened, block by block:

- `.where_eq("owner", json!("alice"))` adds an equality predicate; it is sugar
  for `.add(Predicate { field, op: Op::Eq, value })`.
- `.add(Predicate { … op: Op::Gte … })` adds the `balance >= 100_000` predicate
  explicitly — every operator is reachable this way.
- `.order_by("version", Direction::Desc)` appends a sort order (newest first).
- `.paged(0, 20)` sets a zero-based page window — page `0`, size `20`.
- `to_sql()` returns the `(where_clause, args)` pair. Two predicates produced two
  bound arguments, which is what the `assert_eq!(args.len(), 2)` confirms.

The operators on `Op` cover `Eq`, `Ne`, `Lt`, `Lte`, `Gt`, `Gte`, `Like`,
`ILike`, `In`, and `IsNil`. `IsNil` renders `IS NULL` and consumes **no**
argument slot, so a predicate list and its argument list always stay aligned.

> **Note** `Filter::to_sql()` renders the PostgreSQL default (`$1` placeholders,
> `"id"` quoting). `Filter::to_sql_with(&dialect)` renders the *same* query tree
> for another backend — you meet `SqlDialect` in Step 6, where it is the seam
> that makes a single query string run on three databases.

> **Tip** **Checkpoint.** Drop that snippet into a `#[test]` and run it. Both
> assertions pass: the clause contains `WHERE`, and exactly two values were
> bound. You now have a query value the adapters in Step 6 know how to execute.

## Step 3 — Read the `Page<T>` envelope

A query that pages needs a stable shape to return. `Page<T>` is the canonical
paged-result envelope with a versioned JSON layout, so any client that honors the
contract deserializes it uniformly — a generated SDK consumes it without
per-service handling:

```rust,ignore
pub struct Page<T> {
    pub content: Vec<T>,
    pub number: usize,       // zero-based page index
    pub size: usize,
    pub total_elements: u64,
    pub total_pages: usize,  // derived from total_elements / size
}
```

What just happened: a *list wallets* endpoint — a natural Lumen extension —
returns a `Page<WalletView>` so a client can page through accounts without ever
loading the whole table. `content` carries this page's rows; `number` / `size`
echo the requested window; `total_elements` and `total_pages` let a UI render a
pager.

> **Note** `Page<T>` is the *response* side of paging — what comes back. There is
> also a *request* side, `Pageable` (page number, size, sort), which you meet in
> Step 5. Keep them straight: a caller sends a `Pageable`, a count-aware
> repository returns a `Page<T>`.

## Step 4 — Meet the repository contract

`ReadModel` is a two-method subset of a real framework contract. There are two,
sharing the same idea at different layers.

The **blocking** port, `Repository<T, K>`, is the object-safe `async_trait`
contract; `MemoryRepository` implements it for tests, and an adapter backs it
with a driver in production:

```rust,ignore
#[async_trait]
pub trait Repository<T, K>: Send + Sync {
    async fn find_by_id(&self, id: &K) -> Result<T, DataError>;
    async fn find(&self, filter: &Filter) -> Result<Page<T>, DataError>;
    async fn save(&self, entity: T) -> Result<T, DataError>;
    async fn delete(&self, id: &K) -> Result<(), DataError>;
    // find_page(&Pageable), count, … with defaults
}
```

On top of it, `firefly-data` adds the **reactive** CRUD surface, built on
`Mono` / `Flux`. It is purely additive — nothing about the blocking `Repository`
API changes:

| Method                       | Returns                                     |
|------------------------------|---------------------------------------------|
| `find_all()`                 | `Flux<T>`                                    |
| `find_all_by_id(ids)`        | `Flux<T>`                                    |
| `find_by_id(id)`             | `Mono<T>`                                    |
| `exists_by_id(id)`           | `Mono<bool>`                                 |
| `save(e)`                    | `Mono<T>`                                    |
| `save_all(es)`               | `Flux<T>`                                    |
| `delete_by_id(id)`           | `Mono<()>`                                   |
| `delete_all()`               | `Mono<()>`                                   |
| `count()`                    | `Mono<u64>`                                  |
| `Specification` + `Pageable` | `ReactiveSpecificationRepository` (Step 5)   |

What just happened: these are Spring Data's `ReactiveCrudRepository<T, ID>`
methods, name for name. One detail matters for the rest of the chapter — a "no
row" `find_by_id` resolves to an **empty** `Mono`, the reactive equivalent of
Lumen's `ReadModel::find` returning `None`.

> **Note** **Key term — `block()` / `collect_list()`.** The publishers are lazy:
> nothing runs until you drive them. In an `async` context `Mono::block().await`
> drives a `Mono` to its result, returning `Result<Option<T>, FireflyError>` —
> `Ok(None)` is the empty-`Mono` miss. `Flux::collect_list()` gathers a stream
> into a `Mono<Vec<T>>`, so `flux.collect_list().block().await` returns
> `Result<Option<Vec<T>>, _>`. (A `Mono<T>` also implements `IntoFuture`, so you
> can `repo.save(x).await` directly when you prefer.)

This is the contract Lumen's `ReadModel` is a hand-rolled subset of. Lower it
onto `ReactiveCrudRepository<WalletView, String>` and `find_by_id` / `save` /
`count` come from the framework. The next step does exactly that, in memory.

## Step 5 — Drive the reactive surface in memory

`ReactiveMemoryRepository` is the reactive twin of `MemoryRepository` — the
no-infrastructure way to exercise the real reactive API. It is the reactive
version of Lumen's read store, holding wallet views:

```rust
use firefly_data::{ReactiveCrudRepository, ReactiveMemoryRepository};

#[derive(Clone, PartialEq, Debug)]
struct WalletView { id: String, owner: String, balance: i64, version: i64 }

#[tokio::main]
async fn main() {
    // The closure tells the repository how to read an entity's id.
    let repo = ReactiveMemoryRepository::new(|w: &WalletView| w.id.clone());

    // save -> Mono<T>, driven with block().
    repo.save(WalletView { id: "wlt_1".into(), owner: "alice".into(), balance: 1000, version: 1 })
        .block().await.unwrap();

    // find_all -> Flux<T>, collected to a Vec.
    let all = repo.find_all().collect_list().block().await.unwrap().unwrap();
    assert_eq!(all.len(), 1);

    // find_by_id miss -> empty Mono (Lumen's `ReadModel::find` returning None).
    assert_eq!(repo.find_by_id("ghost".into()).block().await.unwrap(), None);

    // count -> Mono<u64>.
    assert_eq!(repo.count().block().await.unwrap(), Some(1));
}
```

What just happened, line by line:

- `ReactiveMemoryRepository::new(|w| w.id.clone())` builds an empty store whose
  ids are derived by the keyer closure.
- `repo.save(...).block().await.unwrap()` drives the `save` `Mono` to completion;
  the `unwrap()` discards the `Result`, the inner `Some(view)` is the persisted
  value.
- `repo.find_all().collect_list().block().await.unwrap().unwrap()` chains the
  three reactive operators: `find_all()` returns a `Flux`, `collect_list()` folds
  it into a `Mono<Vec<_>>`, `block().await` drives it. The first `unwrap()`
  unwraps the `Result`, the second unwraps the `Option<Vec<_>>`.
- `repo.find_by_id("ghost".into()).block().await.unwrap()` is the miss case — it
  resolves to `None`, the empty-`Mono` contract from Step 4.

Why it matters: swapping `ReadModel` for this repository is mechanical. `upsert`
becomes `save`, `find` becomes `find_by_id(...).block().await`, and the
`GetWallet` handler keeps its `Option<WalletView>` shape. You have just proven
the seam with no database in the loop.

### Sorting and paging come for free

`ReactiveSortingRepository<T, ID>` adds whole-collection sorting and paging —
`find_all_sorted(RequestSort) -> Flux<T>` and `find_all_paged(Pageable) ->
Flux<T>` — and you write **no** code for it. It is a blanket `impl` over any
repository that is both a `ReactiveCrudRepository` and a
`ReactiveSpecificationRepository`, so every `ReactiveMemoryRepository` and every
SQL repository acquires it automatically.

> **Note** **Key term — `Pageable` / `RequestSort`.** These are the *request*
> side of paging (Spring's `Pageable` / `Sort`). `RequestSort::by(["owner"])`
> sorts ascending by a field; `RequestSort::of([Order::desc("id")])` builds an
> explicit order list. `Pageable::of(page, size, sort)` bundles them — and
> crucially, **`page` is 1-based** and the call returns a `Result` (an
> out-of-range page is an error, not a panic), so you `.unwrap()` / `?` it.

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

    // find_all_sorted(RequestSort) — ordered by owner ascending, streamed as a Flux.
    let sorted = repo
        .find_all_sorted(RequestSort::by(["owner"]))
        .collect_list().block().await.unwrap().unwrap();
    assert_eq!(sorted[0].owner, "alice");

    // find_all_paged(Pageable) — page 1 (1-based), size 2, sorted; a Flux window.
    let page = repo
        .find_all_paged(Pageable::of(1, 2, RequestSort::by(["owner"])).unwrap())
        .collect_list().block().await.unwrap().unwrap();
    assert_eq!(page.len(), 2);
}
```

What just happened: `find_all_sorted` ran a match-all `Specification` with the
sort projected onto it; `find_all_paged` ran the same with a `LIMIT`/`OFFSET`
window. Note `Pageable::of(1, 2, …).unwrap()` — page `1` is the *first* page, and
the `unwrap()` handles the `Result`.

> **Note** `find_all_paged` streams the page as a `Flux` window rather than
> buffering a `Page<T>` envelope. Reach for `Page<T>` (Step 3) plus a count query
> when you actually need totals; reach for the streaming window when you do not.

> **Tip** **Checkpoint.** Run both `main`s above (each as a small binary or
> `#[tokio::test]`). The first asserts a save/find/count round-trip and an
> empty-`Mono` miss; the second asserts sort-then-page ordering. Every reactive
> repository in the rest of the chapter behaves identically — only the storage
> behind it changes.

## Step 6 — Lower onto a real, streaming SQL repository

The in-memory repository proves the *shape*. Now make it durable. The relational
adapter, `firefly-data-sqlx`, serves PostgreSQL, MySQL, and SQLite from one
codebase, and **SQLite-in-memory is the no-infrastructure default** — the same
role the in-memory map plays in `samples/lumen`, but exercising the real adapter.

> **Note** **Key term — `Db` enum.** `Db` tags a connection pool with its
> backend: `Db::Postgres(PgPool)`, `Db::MySql(MySqlPool)`, `Db::Sqlite(SqlitePool)`.
> The repository picks the matching `SqlDialect` at runtime from that tag — so
> "new relational database" is a new pool, not a new adapter.

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
    // RowMapper: decode a WalletView from each row — backend-agnostic via AnyRow.
    |row: &AnyRow| Ok::<_, FireflyError>(WalletView {
        id: row.get_str("id")?,
        owner: row.get_str("owner")?,
        balance: row.get_i64("balance")?,
    }),
    // RowWriter: the entity's (column, value) pairs for the upsert.
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

What just happened, by argument:

- `Db::Sqlite(pool)` tags the SQLite pool; the repository reads its backend from
  the tag.
- `TableConfig::new("wallet_views", "id", ["id", "owner", "balance"])` names the
  table, its id column, and the columns to project — the `RowMapper` must decode
  rows shaped by exactly these columns.
- The **`RowMapper`** closure decodes one row. `AnyRow` is the backend-agnostic
  row wrapper; `get_str` / `get_i64` read columns by name, so the same closure
  works on all three relational backends.
- The **`RowWriter`** closure produces the `(column, value)` pairs the adapter
  renders into a dialect-aware `UPSERT` (`ON CONFLICT … DO UPDATE` for
  Postgres/SQLite, `ON DUPLICATE KEY UPDATE` for MySQL).

Why it matters: switching to Postgres or MySQL is `Db::Postgres(pg_pool)` /
`Db::MySql(my_pool)` — the repository call sites do not change. That is the
upgrade path the `samples/lumen` comment promises: the in-memory `ReadModel`
becomes a `SqlxReactiveRepository<WalletView, String>`, and the `GetWallet`
handler is none the wiser. Reads stream off sqlx's row stream into a `Flux`, so a
million-row table never lands fully in memory.

> **Design note.** This is hexagonal architecture — ports & adapters. Your
> service depends on the repository *port* (`ReactiveCrudRepository`); the
> *adapter* (the relational crate, the Mongo crate) is chosen at wiring time.
> Swapping Postgres for MySQL is a pool change, not a code change — the call
> sites never move. Adding a *new* database is "write a `firefly-data-<tech>`
> crate that implements the ports," not "rewrite the data layer." `firefly-data`
> ships three `SqlDialect` impls (`PostgresDialect`, `MySqlDialect`,
> `SqliteDialect`) and a `Specification::to_mongo()` lowering, so the same query
> tree renders correctly per backend.

> **Tip** **Checkpoint.** Run that snippet as a test. It creates a `wallet_views`
> table in `sqlite::memory:`, saves a row through the real adapter, and returns
> with no error — the production read store, exercised end to end with zero
> external infrastructure.

The constructor takes a `Db`, a `TableConfig`, a `RowMapper`, and a `RowWriter`;
three chainable builders add cross-cutting behaviour, each returning a fresh
repository:

- `.with_auditor(Auditor)` — stamps `created_at` / `updated_at` / `created_by` /
  `updated_by` on every write (insert vs update is decided by whether the row
  already exists): automatic auditing.
- `.with_soft_delete(SoftDeletePolicy)` — hides soft-deleted rows from every read
  and turns `delete_by_id` into a `deleted_at` stamp instead of a physical
  `DELETE`: logical (soft) delete.
- `.with_version_column("version")` — turns on optimistic locking (Step 8).

## Step 7 — Declare the repository the Spring Data way

You rarely build the repository by hand the way Step 6 did. For a typed entity,
two derives give you the Spring Data "declare a repository, get the
implementation" experience. This is exactly how the
[`lumen-ledger`](./22-layered-microservices.md) sample wires its persistence.

> **Note** **Key term — `#[derive(Entity)]`.** This derive generates an entity's
> `@Table` / `@Id` / `@Version` / `@Column` mapping from its fields. Scalar
> columns (`String`, `i64`, `Uuid` as text, `DateTime<Utc>` as text) map
> automatically; a non-scalar field (a typed enum) uses
> `#[firefly(with(read = "...", write = "..."))]` to name its converters — the
> `@Enumerated(STRING)` boundary.

```rust,ignore
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, firefly::Entity)]
#[firefly(table = "wallets")]
pub struct Wallet {
    #[firefly(id)]
    pub id: Uuid,
    pub account_number: String,
    pub owner: String,
    pub balance: i64,
    pub currency: String,
    // A typed enum maps through explicit converters — @Enumerated(STRING).
    #[firefly(with(read = "WalletStatus::from_token", write = "WalletStatus::as_str"))]
    pub status: WalletStatus,
    // Optimistic-locking version (@Version) — bumped by the store on update.
    #[firefly(version)]
    pub version: i64,
    // Audit stamps, managed by the store's Auditor.
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Then `#[derive(SqlxRepository)]` over a struct holding the entity's
`SqlxReactiveRepository`:

```rust,ignore
use firefly::data::{DataError, Pageable};
use firefly::data_sqlx::SqlxReactiveRepository;
use uuid::Uuid;

#[derive(firefly::SqlxRepository)]
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}
```

What just happened: `#[derive(SqlxRepository)]` registers `WalletRepository` as a
`@Repository` bean **built from the injected `Db` datasource** (wiring the
entity's `@Version` locking and `@CreatedDate`/`@LastModifiedDate` auditing), and
implements `ReactiveCrudRepository` by delegation. There is no `#[bean]` factory
and no hand-written CRUD — the derive builds the inner `SqlxReactiveRepository`
from the autowired `Db`, exactly like Spring Data's
`interface WalletRepository extends ReactiveCrudRepository<Wallet, UUID>`.

> **Note** **Key term — `Uuid` (any) id.** The repository's `ID` is unbounded,
> like Spring Data's `CrudRepository<T, ID>`: the sqlx adapter accepts any
> `serde::Serialize` key through its `SqlKey` trait, so a `Uuid`, `i64`,
> `String`, an enum, or a composite-key struct all work with no newtype dance.
> The key binds as its serde-JSON form against the id column.

### Derived and custom queries — `#[firefly::repository]`

Beyond CRUD, the `#[firefly::repository]` macro derives a query straight from a
*method name*: `find_by_owner(&str)` becomes `WHERE owner = ?`. Apply it to an
`impl` block of typed stub methods; the macro discards the placeholder body
(`unimplemented!()`) and generates a real one that marshals the arguments and
delegates to the runtime engine. The **return type selects the operation**:

| Return shape                    | Generated call            |
|---------------------------------|---------------------------|
| `Result<Vec<T>, DataError>`     | `find_by_derived`         |
| `Result<Option<T>, DataError>`  | `find_by_derived` (first) |
| `Result<i64, DataError>`        | `count_by_derived`        |
| `Result<bool, DataError>`       | `exists_by_derived`       |
| `Result<u64, DataError>`        | `delete_by_derived`       |

This is the actual derived-query block on `lumen-ledger`'s `WalletRepository`:

```rust,ignore
use firefly::data::{DataError, Pageable};

#[firefly::repository]
impl WalletRepository {
    /// `SELECT … WHERE owner = ?` — every wallet of one owner.
    pub async fn find_by_owner(&self, owner: &str) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }

    /// `SELECT COUNT(*) WHERE status = ?`.
    pub async fn count_by_status(&self, status: &str) -> Result<i64, DataError> {
        unimplemented!()
    }

    /// Paged `SELECT … WHERE status = ?` — a trailing `Pageable` makes it paged.
    pub async fn find_by_status(
        &self,
        status: &str,
        page: Pageable,
    ) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }
}
```

What just happened: the method *names* are the grammar — prefix (`find` / `count`
/ `exists` / `delete`), then `By`, then a chain of `And` / `Or` property
conditions (`find_by_owner_and_status`). A method whose **last argument is a
`Pageable`** is a *paged* derived query: the pageable's sort and window are
appended to the generated `WHERE`. Build the page with
`Pageable::of(page, size, sort)` — remember `page` is **1-based** and the call
returns a `Result`:

```rust,ignore
use firefly::data::{Pageable, RequestSort};

# async fn ex(wallets: &WalletRepository) -> Result<(), firefly::data::DataError> {
let page = Pageable::of(1, 20, RequestSort::by(["account_number"])).unwrap();
let active = wallets.find_by_status("active", page).await?;
# Ok(())
# }
```

When the name grammar can't express the query, annotate the stub with
`#[query(...)]` and write the SQL yourself. A `:name` placeholder binds the
argument named `name`, and the return type selects the operation exactly as for
derived methods — `Vec<T>` / `Option<T>` for a list, `i64` for a count, `bool`
for an exists, `u64` for a *modifying* statement (the affected-row count):

```rust,ignore
use firefly::data::DataError;

#[firefly::repository]
impl WalletRepository {
    // Native SQL; :status binds to the `status` argument.
    #[query("SELECT id, owner FROM wallets WHERE status = :status ORDER BY id DESC")]
    async fn list_by_status(&self, status: &str) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }

    // u64 return -> a modifying statement; the value is the affected-row count.
    #[query("UPDATE wallets SET status = :to WHERE status = :from")]
    async fn retire(&self, from: &str, to: &str) -> Result<u64, DataError> {
        unimplemented!()
    }
}
```

What just happened: `#[query("…")]` is shorthand for `#[query(sql = "…")]`
(native SQL). For a portable, entity-oriented query use the JPQL-like form
`#[query(jpql = "…", entity = "Wallet")]`, whose `FROM <Entity>` is transpiled to
the configured table so the same string runs on Postgres, MySQL, or SQLite. Under
the hood, the method-name parser lowers a derived query through the active
`SqlDialect`, and `#[query]` lowers to the repository's
`query_list` / `query_count` / `query_exists` / `query_execute` helpers.

> **Tip** Reach for the method-name grammar for simple predicates and a trailing
> `Pageable` for paged reads; use `#[query(...)]` for anything the name grammar
> can't express. A relational `Account` / `Order` service writes these; Lumen's
> own event-sourced read side stays a hand-rolled in-memory `ReadModel` because
> its query needs are exactly two methods.

## Step 8 — Turn on optimistic locking

A versioned entity needs lost-update protection. Naming the version column on the
repository turns a `save` into a **version-guarded conditional upsert**: every
write bumps the version and guards the conflict-update on the version the entity
was loaded with (`WHERE version = <loaded>`). If a concurrent writer moved the
stored version on, the guarded update matches zero rows and the save is rejected
instead of silently overwriting the other change.

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

What just happened: `.with_version_column("version")` made every `save`
conditional on the loaded version. The blocking `SqlxRepository::save` surfaces a
stale write as `DataError::OptimisticLock`; the reactive `save` surfaces it
through its `FireflyError` channel (a 409), which
`firefly_data_sqlx::is_optimistic_lock(&err)` detects so a service can map it to
a domain conflict. (Conflict detection is enforced on Postgres and SQLite; on
MySQL the version is bumped but the guard is not applied.)

> **Note** A stale write fails rather than silently overwriting a concurrent
> change; the caller reloads and retries. Lumen's *write* side reaches the same
> lost-update guarantee differently — through the event store's
> optimistic-concurrency `append` (see
> [Event Sourcing](./11-event-sourcing.md)) — but a relational `Account` /
> `Order` repository uses a version column.

> **Tip** **Checkpoint.** `lumen-ledger`'s
> `models/src/repositories/wallet/v1/wallet_repository.rs` has a test,
> `optimistic_locking_rejects_a_stale_write`, that loads a row twice, writes once
> through each handle, and asserts the second is an
> `is_optimistic_lock` conflict. Run it: `cargo test -p lumen-ledger-models`.

## Step 9 — Build the pool from configuration

In every snippet so far the pool was constructed by hand. In a real service the
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

What just happened: the **URL scheme selects the backend** (each behind its cargo
feature): `postgres://` / `postgresql://` → PostgreSQL, `mysql://` → MySQL,
`sqlite:` → SQLite. So a config change from `postgres://…` to `mysql://…` moves
the whole service to MySQL with no code edit — the pluggable-database promise,
driven from configuration.

Three entry points build the `Db`:

- `Db::connect(url).await -> Result<Db, FireflyError>` — a pool from a URL, using
  driver defaults.
- `Db::connect_with(&props).await` — a pool honouring the full
  `DataSourceProperties` (sizes, timeouts, lifetimes).
- `data_sqlx::auto_configure(&props).await` — the **one-call startup path**: it
  builds the pool *and* registers a `SqlxTransactionManager`, so
  `#[firefly::transactional]` resolves its manager with no manual wiring. The
  returned `Db` then builds your typed repositories.

The shape of a boot sequence is: load config → bind `DataSourceProperties` →
`await auto_configure` once → build repositories from the returned `Db`:

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

What just happened: `auto_configure(&props)` did the two startup jobs at once —
built the pool and registered the `SqlxTransactionManager` in the process — so
the transaction boundary in Step 10 needs no extra wiring.

> **Design note.** Configuration drives the runtime, not a container. A plain
> `serde` struct is bound from `firefly.datasource.*`, and a single awaited
> `auto_configure` builds the pool and registers the transaction manager — the
> wiring is explicit, compiler-checked, and visible in one place rather than
> assembled by reflection at runtime.

### Schema migrations

The table those repositories read needs to exist first. `firefly-migrations` is a
forward-only SQL migration runner. Files are named `V{version}__{description}.sql`
(e.g. `V001__init.sql`); each runs once, in version order, inside a transaction:

```rust,ignore
use firefly_migrations::{run, DirSource};

let src = DirSource { dir: "migrations".into() };
run(&mut db, &src)?;                                       // applies pending migrations in order
let status = firefly_migrations::inspect(&mut db, &src)?;  // applied + pending
```

The [CLI](./19-cli.md) wraps this: `firefly db init`,
`firefly db migrate -m "create wallet_views"`,
`firefly db upgrade --url sqlite://lumen.db`, and `firefly db status`. A
`V001__wallet_views.sql` creating the read-model table is all the schema Lumen's
durable read side needs.

## Step 10 — Make a multi-write change atomic

A single `save` is atomic on its own. A *transfer* — debit one account, credit
another, write two ledger entries — must be atomic as a whole: all four writes
commit, or none do. That is what `#[firefly::transactional]` is for.

> **Note** **Key term — `#[firefly::transactional]`.** Annotate an `async fn`
> that returns `Result<_, E>` (where `E: From<TxError>`) and the body runs inside
> a transaction — **commit on `Ok`, rollback on `Err`**. This is Spring's
> `@Transactional`, made declarative in Rust.

```rust,ignore
use firefly::transactional::TxError;
use firefly_data_sqlx::SqlxReactiveRepository;

#[derive(Debug, Clone)] struct Account { id: String, balance: i64 }
#[derive(Debug, Clone)] struct Entry   { id: String, account: String, delta: i64 }

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

What just happened, and why it is seamless:

> **Note** **Key term — ambient enlistment.** While a transactional scope is
> active, the manager stows the open transaction in a task-local. Every
> `SqlxReactiveRepository` / `SqlxRepository` write *inside the fn* routes onto
> that active transaction instead of a fresh pool connection. So a plain sequence
> of `repo.save(...).await?` calls is atomic with **no change to the repository
> code** — you do not thread a connection or a `&mut Tx` through every call.

The attribute accepts the full Spring vocabulary: `propagation` (`required` /
`requires_new` / `nested` / `supports` / `not_supported` / `mandatory` /
`never`), `isolation` (`read_committed` / `repeatable_read` / `serializable` /
…), `read_only`, `timeout_ms`, and `manager = "<expr>"` — Spring's
`@Transactional("txManager")`, which runs against an explicit
`TransactionManager` (e.g. `self.tx_manager()`) instead of the process-global
registry. This is exactly what `lumen-ledger`'s `WalletServiceImpl::transfer_tx`
does:

```rust,ignore
// lumen-ledger/core/src/services/wallet/v1/wallet_service_impl.rs (excerpt)
#[firefly::transactional(manager = "self.tx_manager()")]
async fn transfer_tx(&self, from: Uuid, to: Uuid, amount: i64)
    -> Result<WalletResponse, ServiceError>
{
    // … preconditions checked before any write …
    let saved_source = self.persist(source).await?;   // debit
    self.persist(dest).await?;                         // credit — if this fails,
    Ok(saved_source)                                   // the debit rolls back
}
```

### Rollback rules — naming a pattern, not an exception type

By default every `Err` rolls back. Spring names exception *types* to refine that;
because Rust's `Result` already separates failure from success, the Firefly
analog names an error **pattern** (any match pattern for the fn's error type,
alternatives `A | B` included). Then:

- `no_rollback_for = "P"` — **Spring's `noRollbackFor`**: an `Err` matching `P`
  **commits** instead of rolling back;
- `rollback_only_for = "P"` — roll back **only** for errors matching `P`,
  committing the rest;
- with both, `no_rollback_for` wins on overlap.

```rust,ignore
// Persist the audit row even when the domain rejects the charge, but still roll
// back on any infrastructure failure — @Transactional(noRollbackFor = …).
#[firefly::transactional(no_rollback_for = "BillingError::Rejected(_)")]
async fn charge(&self, req: Charge) -> Result<Receipt, BillingError> {
    self.audit.save(/* … */).await?;        // committed even on a Rejected error
    self.gateway.settle(req).await          // a Backend error still rolls back
}
```

> **Warning** There is no `rollback_for`. Spring's `rollbackFor` is *additive* —
> it adds exception types to the runtime-exceptions that already roll back. Rust
> has no checked/unchecked split (every `Err` rolls back by default), so an
> additive rule would be a no-op. `rollback_only_for` is therefore named to
> signal that it *restricts* (rather than widens) the rollback set, so a Spring
> port is never silently inverted. Writing `rollback_for` is a friendly compile
> error pointing you at the two rules above.

For programmatic control there is `firefly::transactional::transactional(opts, f)`
and `transactional_on(&manager, opts, f)` for an explicit manager, with
`TxOptions`, `Propagation`, and `Isolation` builders. The sqlx adapter —
`SqlxTransactionManager`, registered once at startup (the `auto_configure` path
in Step 9 does this for you) — supplies the real behaviour: full propagation,
isolation, read-only, a statement timeout, and `SAVEPOINT`-based `NESTED`
nesting.

> **Tip** **Checkpoint.** The partial-write protection is proven end-to-end in
> `firefly-data-sqlx`'s `tests/transactional.rs` and in `lumen-ledger`'s service
> tests: a transfer whose credit fails after the debit leaves *both* accounts
> unchanged. Run `cargo test -p lumen-ledger-core` to watch it.

### Document store — `firefly-data-mongodb`

The same ports reach a document database. `MongoRepository<T, ID>` puts a MongoDB
collection behind the **same** `ReactiveCrudRepository` +
`ReactiveSpecificationRepository` traits, lowering a `Specification` via
`Specification::to_mongo()` exactly as the relational adapters lower it via
`to_sql`. A `BaseDocument` mixin (embedded with `#[serde(flatten)]`) carries the
audit stamps and soft-delete column, and reads stream lazily off the driver
cursor as a `Flux`:

```rust,no_run
use firefly_data::ReactiveCrudRepository;
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

What just happened: because all four backends sit behind the same ports, a
service that codes against `ReactiveCrudRepository` / `Specification` moves from
Postgres to MySQL, SQLite, or MongoDB by swapping the adapter constructor.

> **Note** Lumen pulls none of these adapters in its default build — they are
> opt-in cargo features on the `firefly` facade
> (`firefly = { version = "26.6", features = ["data-sqlx"] }`), re-exported as
> `firefly::data_sqlx` / `firefly::data_mongodb`. The teaching build stays lean;
> the production build adds exactly the driver it needs. This is the same
> one-dependency story from [Quickstart](./02-quickstart.md): no starter to
> forget, no version skew.

## Recap

Lumen now has a clear persistence story, even though its teaching build stays
infrastructure-free:

- **The read store is a repository.** `ReadModel` (in
  `samples/lumen/src/ledger.rs`) is a `#[derive(Repository)]` data-access bean
  wrapping a `Mutex<HashMap<String, WalletView>>` and exposing exactly
  `upsert(view)` and `find(id)` — the two operations the query side needs. The
  `GetWallet` handler depends on the *contract*, not the map.
- **The baseline is in-memory by choice.** The map keeps the dependency footprint
  at one Firefly crate; the source comment names the upgrade explicitly.
- **The upgrade is an adapter swap.** Lowering `ReadModel` onto
  `ReactiveCrudRepository<WalletView, String>` — `SqlxReactiveRepository` for
  Postgres/MySQL/SQLite, `MongoRepository` for Mongo — turns `upsert` into `save`
  and `find` into `find_by_id`, with the handler's `Option<WalletView>` shape
  unchanged. A new database is a new pool (relational) or a new adapter crate,
  never a rewrite.

You also now know:

- How to compose a `Filter`, render it with `to_sql()`, and read a `Page<T>`.
- That the reactive surface returns `Mono` / `Flux`; you drive them with
  `block().await` (→ `Result<Option<T>, _>`) and `collect_list()`, and a miss is
  an empty `Mono`.
- That `Pageable::of(page, size, sort)` is **1-based** and returns a `Result`.
- That `#[derive(Entity)]` + `#[derive(SqlxRepository)]` give you a Spring
  Data-style repository, and `#[firefly::repository]` adds derived and
  `#[query(...)]` queries.
- That `with_version_column` is `@Version` optimistic locking, that
  `auto_configure` builds the pool and registers the transaction manager in one
  awaited call, and that `#[firefly::transactional]` makes a multi-write change
  atomic through ambient enlistment — with rollback *patterns*, not
  `rollback_for`.

## Exercises

1. **Re-skin `ReadModel` as a trait.** Define
   `trait WalletViews { fn upsert(&self, v: WalletView); fn find(&self, id: &str) -> Option<WalletView>; }`,
   implement it for the in-memory `ReadModel`, and have the `GetWallet` handler
   take `&dyn WalletViews`. Confirm the rest of Lumen still compiles — proof the
   query side depends on the contract, not the map.

2. **Back the read model with SQLite.** Using the `SqlxReactiveRepository`
   listing from Step 6, create a `wallet_views` table in `sqlite::memory:`,
   `save` two views, and `find_all().collect_list()` them. Assert both come back.
   This is the production read store, exercised end to end against the real
   adapter with no external infrastructure.

3. **Page the wallets.** Build a `Filter` that selects wallets with
   `balance >= 100_000` ordered by `version` descending, page `(0, 20)`, and
   print its `to_sql()`. Then describe (one sentence each) how the same filter
   would render under `MySqlDialect` and `SqliteDialect` via `to_sql_with`.

4. **Add a derived query.** On a `#[derive(SqlxRepository)]` repository, add a
   `#[firefly::repository]` method `find_by_owner(&self, owner: &str) ->
   Result<Vec<Wallet>, DataError>`, then a paged variant
   `find_by_status(&self, status: &str, page: Pageable) -> Result<Vec<Wallet>, DataError>`.
   Build the `Pageable` with `Pageable::of(1, 20, RequestSort::by(["id"]))` and
   confirm it compiles. Note that `page` is the *first* page, not the second.

5. **Trace the swap.** List the exact lines in `samples/lumen/src/ledger.rs` that
   would change if `ReadModel` became a `SqlxReactiveRepository<WalletView,
   String>`, and which lines in the `GetWallet` handler would *not* — confirming
   the adapter boundary holds.

## Where to go next

- Model the business itself — the `Money` value object and the `Wallet`
  aggregate — in **[Domain-Driven Design](./08-domain-driven-design.md)**.
- See how the read store and the write store are split, and how the query side
  reads from the repository you just framed, in **[CQRS](./09-cqrs.md)**.
- Watch the projection that *writes* to `ReadModel` come alive in
  **[EDA & Messaging](./10-eda-messaging.md)** and
  **[Event Sourcing](./11-event-sourcing.md)**.
- See the full Spring Data-style persistence layer wired across crates in
  **[Layered Microservices](./22-layered-microservices.md)**.
