# Persistence & Reactive Repositories

`firefly-data` provides the framework's **persistence vocabulary**: a composable
`Filter` query DSL, a `Page<T>` paged-result envelope, a blocking
`Repository<T, K>` contract, and — built on `firefly-reactive` — a **reactive
CRUD surface** that is the Rust analog of Spring Data R2DBC. This chapter covers
both, with a real streaming Postgres repository.

## The `Page<T>` envelope

`Page<T>` is the canonical paged result, wire-identical to the Java/.NET/Go
`Page<T>` so SDK clients deserialize it uniformly:

```rust,ignore
pub struct Page<T> {
    pub content: Vec<T>,
    pub number: usize,       // zero-based page index
    pub size: usize,
    pub total_elements: u64,
    pub total_pages: usize,  // derived
}
```

## The `Filter` DSL

A `Filter` composes predicates, sorts, and a page window, and renders to a
parameterised `WHERE` clause via `to_sql()`:

```rust
use firefly_data::{Direction, Filter, Op, Predicate};
use serde_json::json;

let filter = Filter::default()
    .where_eq("status", json!("OPEN"))
    .add(Predicate { field: "total".into(), op: Op::Gte, value: json!(100) })
    .order_by("created_at", Direction::Desc)
    .paged(0, 20);

let (where_clause, args) = filter.to_sql();
// where_clause: a parameter-indexed " WHERE ..." fragment
// args:         the bound values, in order
assert!(where_clause.contains("WHERE"));
assert_eq!(args.len(), 2);
```

The operators (`Op`) cover `Eq`, `Ne`, `Lt`, `Lte`, `Gt`, `Gte`, `Like`,
`ILike`, `In`, and `IsNil` — the last skips an argument slot, so a predicate
list and its argument list stay aligned.

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

## The reactive CRUD surface

On top of the blocking contract, `firefly-data` adds a **reactive** surface —
the Spring Data R2DBC analog — built on `Mono` / `Flux`. It is purely additive:
nothing about the existing `Repository` API changes.

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
exactly as Spring Data signals a missing `findById`.

### In-memory, for tests

`ReactiveMemoryRepository` is the reactive twin of `MemoryRepository`. Drive the
publishers with `block()` / `collect_list()`:

```rust
use firefly_data::{ReactiveCrudRepository, ReactiveMemoryRepository};

#[derive(Clone, PartialEq, Debug)]
struct User { id: String, name: String }

#[tokio::main]
async fn main() {
    let repo = ReactiveMemoryRepository::new(|u: &User| u.id.clone());

    // save -> Mono<T>
    repo.save(User { id: "u1".into(), name: "alice".into() })
        .block().await.unwrap();

    // find_all -> Flux<T>, collected to a Vec
    let all = repo.find_all().collect_list().block().await.unwrap().unwrap();
    assert_eq!(all.len(), 1);

    // find_by_id miss -> empty Mono
    assert_eq!(repo.find_by_id("ghost".into()).block().await.unwrap(), None);
    assert_eq!(repo.count().block().await.unwrap(), Some(1));
}
```

### Real Postgres, streaming rows as a `Flux`

`PostgresReactiveRepository` is the production repository over `tokio-postgres`.
Reads drive the driver's `query_raw` **row stream**, so each row is decoded by a
`RowMapper` and emitted the moment it arrives over the wire — a million-row
table never lands fully in memory. Writes use a per-entity `inserter` closure
that renders a `T` to an upsert whose `RETURNING` projects the configured
columns.

```rust,no_run
use std::sync::Arc;
use firefly_data::{PostgresReactiveRepository, ReactiveCrudRepository, TableConfig};
use firefly_kernel::FireflyError;
use tokio_postgres::{Row, types::ToSql, NoTls};

#[derive(Clone, PartialEq, Debug)]
struct User { id: String, name: String }

# async fn ex() -> Result<(), Box<dyn std::error::Error>> {
let (client, conn) =
    tokio_postgres::connect("postgres://localhost/app", NoTls).await?;
tokio::spawn(async move { let _ = conn.await; });
let client = Arc::new(client);

let repo: PostgresReactiveRepository<User, String> = PostgresReactiveRepository::new(
    Arc::clone(&client),
    TableConfig::new("users", "id", ["id", "name"]),
    // RowMapper: decode (id, name) from each streamed row.
    |row: &Row| Ok(User {
        id: row.try_get("id").map_err(|e| FireflyError::internal(e.to_string()))?,
        name: row.try_get("name").map_err(|e| FireflyError::internal(e.to_string()))?,
    }),
    // inserter: upsert RETURNING the projected columns.
    |u: &User| (
        "INSERT INTO \"users\" (\"id\", \"name\") VALUES ($1, $2) \
         ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\" \
         RETURNING \"id\", \"name\"".to_string(),
        vec![
            Box::new(u.id.clone()) as Box<dyn ToSql + Sync + Send>,
            Box::new(u.name.clone()) as Box<dyn ToSql + Sync + Send>,
        ],
    ),
);

// Rows stream lazily out of find_all() as a Flux.
let all = repo.find_all().collect_list().block().await?.unwrap();
# Ok(())
# }
```

Use `stream_query(sql, params)` for custom derived queries: any `SELECT`
projecting the configured columns is streamed row-by-row through the same
`RowMapper`. This `Flux` plugs directly into a `NdJson` / `Sse` endpoint, so a
database read streams to the client end-to-end with backpressure — no
collect-then-emit step anywhere in the path.

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

### Relational — `firefly-data-sqlx` (Postgres / MySQL / SQLite)

`SqlxReactiveRepository` (and the blocking `SqlxRepository`) serve all three
relational backends from one codebase. A `Db` enum tags a `PgPool` /
`MySqlPool` / `SqlitePool` with its `Backend`, and the repository picks the
matching `SqlDialect` at runtime — so "new relational DB = new pool", not "new
adapter". `UPSERT` is dialect-aware (`ON CONFLICT … DO UPDATE` for
Postgres/SQLite, `ON DUPLICATE KEY UPDATE` for MySQL), reads stream off sqlx's
row stream into a `Flux`, and an optional `Auditor` / `SoftDeletePolicy`
auto-stamps and hides rows on every write/read.

```rust
use firefly_data::{ReactiveCrudRepository, TableConfig};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone, PartialEq)]
struct User { id: String, name: String }

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
sqlx::query(r#"CREATE TABLE "users" ("id" TEXT PRIMARY KEY, "name" TEXT NOT NULL)"#)
    .execute(&pool).await.unwrap();

let repo: SqlxReactiveRepository<User, String> = SqlxReactiveRepository::new(
    Db::Sqlite(pool),
    TableConfig::new("users", "id", ["id", "name"]),
    // RowMapper: decode by column name — backend-agnostic via AnyRow.
    |row: &AnyRow| Ok::<_, FireflyError>(User {
        id: row.get_str("id")?,
        name: row.get_str("name")?,
    }),
    // RowWriter: the entity's (column, value) pairs.
    |u: &User| vec![
        ColumnValue::new("id", u.id.clone()),
        ColumnValue::new("name", u.name.clone()),
    ],
);
repo.save(User { id: "u1".into(), name: "alice".into() }).block().await.unwrap();
# });
```

Switching to Postgres or MySQL is `Db::Postgres(pg_pool)` / `Db::MySql(my_pool)`
— the repository call sites do not change.

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
struct UserDocument {
    #[serde(rename = "_id")] id: String,
    name: String,
    #[serde(flatten)] base: BaseDocument,
}

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
let collection = client.database("app").collection::<Document>("users");
let repo: MongoRepository<UserDocument, String> =
    MongoRepository::new(collection, |u: &UserDocument| Bson::String(u.id.clone()));

repo.save(UserDocument {
    id: "u1".into(), name: "alice".into(), base: BaseDocument::new(),
}).block().await?;
# Ok(())
# }
```

Because all four backends sit behind the same ports, a service that codes
against `Repository` / `ReactiveCrudRepository` / `Specification` can move from
Postgres to MySQL, SQLite, or MongoDB by swapping the adapter constructor — and
adding a *new* database is "write a `firefly-data-<tech>` crate that implements
the ports", not "rewrite the data layer". Both adapters are tested against real
Postgres, MySQL, SQLite, and MongoDB.

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
"create users"`, `firefly db upgrade --url sqlite://app.db`, and
`firefly db status`.

## Transactions

`firefly-transactional` provides `with_tx(ctx, db, f)` over pluggable `Database`
/ `Transaction` ports, so a unit of work commits or rolls back as a whole. Bind
the ports to your driver and run your repository calls inside the closure.

With persistence in place, the next chapter models the business itself. See
[Domain-Driven Design](./08-domain-driven-design.md).
