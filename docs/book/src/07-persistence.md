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
