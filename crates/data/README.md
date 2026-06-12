# `firefly-data`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-common-data` (R2DBC) · **Go module:** `data`

## Overview

`firefly-data` provides the **persistence abstractions** every Firefly
service shares — a generic filter DSL that renders to parameterised SQL,
a canonical `Page<T>` paged-result envelope, and a typed
`Repository<T, K>` contract with an in-memory implementation for tests.
Services that talk to PostgreSQL define their own typed repository
conforming to the `Repository<T, K>` trait, using `Filter::to_sql()` to
render the WHERE / ORDER BY / LIMIT clauses.

## Public surface

### `Page<T>`

```rust,ignore
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub content: Vec<T>,
    pub number: usize,        // zero-based page index
    pub size: usize,
    pub total_elements: u64,
    pub total_pages: usize,
}

impl<T> Page<T> {
    pub fn new(content: Vec<T>, number: usize, size: usize, total: u64) -> Self;
    pub fn empty() -> Self;
}
```

The wire shape (`content` / `number` / `size` / `totalElements` /
`totalPages`) is identical to the Java/.NET/Go `Page<T>` so SDK clients
dispatch on the same JSON.

### Filter DSL

```rust,ignore
pub enum Op { Eq, Ne, Lt, Lte, Gt, Gte, Like, ILike, In, IsNil } // "eq" | "ne" | …

pub struct Predicate { pub field: String, pub op: Op, pub value: serde_json::Value }
pub struct Sort { pub field: String, pub direction: Direction } // Asc | Desc

pub struct Filter {
    pub predicates: Vec<Predicate>,
    pub sorts: Vec<Sort>,
    pub page: usize,
    pub size: usize,
}

impl Filter {
    pub fn where_eq(self, field, value) -> Self;
    pub fn add(self, p: Predicate) -> Self;
    pub fn order_by(self, field, direction: Direction) -> Self;
    pub fn paged(self, page: usize, size: usize) -> Self;
    pub fn to_sql(&self) -> (String, Vec<serde_json::Value>); // -> " WHERE …", [args]
}
```

`to_sql` renders a parameterised PostgreSQL fragment using `$1`, `$2`,
… argument placeholders. Identifier quoting is double-quoted. `IsNil`
renders `IS NULL` and skips its argument slot, so the index ↔ argument
mapping stays correct.

### Repository contract

```rust,ignore
#[async_trait]
pub trait Repository<T, K>: Send + Sync {
    async fn find_by_id(&self, id: &K) -> Result<T, DataError>;
    async fn find(&self, filter: &Filter) -> Result<Page<T>, DataError>;
    async fn save(&self, entity: T) -> Result<T, DataError>;
    async fn delete(&self, id: &K) -> Result<(), DataError>;
}

pub enum DataError {
    NotFound,         // "firefly/data: not found" — same message as Go's ErrNotFound
    Backend(String),  // store-specific failure
}
```

The Go port's `context.Context` parameter is implicit in async Rust —
cancellation rides on the future itself.

### `MemoryRepository`

In-process implementation of `Repository`, parameterised on a
user-supplied keyer closure `Fn(&T) -> K`. Honours paging; does not
honour predicates (use a SQL-backed Repository for filtering). Unlike
the Go original it is internally `RwLock`-guarded, so it is
`Send + Sync` and safe to share across tasks.

## Quick start

```rust
use firefly_data::{Direction, Filter, MemoryRepository, Repository};

#[derive(Clone)]
struct User {
    id: String,
    name: String,
}

#[tokio::main]
async fn main() {
    let repo = MemoryRepository::new(|u: &User| u.id.clone());
    repo.save(User { id: "u1".into(), name: "alice".into() })
        .await
        .unwrap();

    let f = Filter::new()
        .where_eq("name", "alice")
        .order_by("id", Direction::Asc)
        .paged(0, 10);
    let page = repo.find(&f).await.unwrap();
    assert_eq!(page.total_elements, 1);
}
```

For a real Postgres-backed repository, implement the `Repository` trait
against your SQL driver, using `f.to_sql()` to render the WHERE /
ORDER BY / LIMIT clauses.

## pyfly parity

On top of the Go-parity surface above, `firefly-data` ports pyfly's
Spring-Data-style data primitives. They stay **storage-agnostic** — no
SQL engine is implied; everything lowers to the existing `Filter` DSL,
renders parameterised SQL fragments, or evaluates in memory.

### `Specification`

```rust,ignore
pub enum Specification { All, Pred(Predicate), And(Vec<Specification>), Or(Vec<Specification>), Not(Box<Specification>) }

impl Specification {
    pub fn all() -> Self;                                   // no-op, matches every row
    pub fn pred(p: Predicate) -> Self;
    pub fn eq(field, value) -> Self;                        // field = value leaf
    pub fn and(self, other) -> Self;                        // also the `&` operator
    pub fn or(self, other) -> Self;                         // also the `|` operator
    pub fn not(self) -> Self;                               // also the `!` operator
    pub fn is_conjunction(&self) -> bool;
    pub fn to_filter(&self) -> Option<Filter>;              // lower a pure-AND tree to Filter
    pub fn to_sql(&self) -> (String, Vec<Value>);           // parenthesised $1,$2,… fragment
    pub fn matches<T: Serialize>(&self, entity: &T) -> bool;// in-memory evaluation
}
```

Composable query predicates (Spring Data's `Specification<T>`), combined
with `&` (AND), `|` (OR), `!` (NOT). A pure conjunction lowers to the
flat AND-only `Filter` via `to_filter`; any tree renders to a
parenthesised parameterised SQL fragment via `to_sql` (no leading
`WHERE`, so it embeds inside a larger clause); and `matches` evaluates a
spec in memory against any `serde`-serialisable entity (supporting `eq`,
`ne`, `<`/`<=`/`>`/`>=`, `like`/`ilike`, `in`, `isnil`).

### `AuditStamps` + `Auditor`

```rust,ignore
#[serde(rename_all = "camelCase")]
pub struct AuditStamps {
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
}

impl Auditor {
    pub fn new() -> Self;                                       // system UTC clock
    pub fn with_clock(clock: impl Fn() -> DateTime<Utc>) -> Self;
    pub fn on_insert(&self, stamps: &mut AuditStamps, user: Option<&str>);
    pub fn on_update(&self, stamps: &mut AuditStamps, user: Option<&str>);
}
```

`on_insert` sets all four fields (created/updated equal); `on_update`
moves only the modification fields. The current user is supplied
explicitly — the Rust idiom for pyfly's implicit
`RequestContext.current()`.

### `SoftDelete` + `SoftDeletePolicy`

```rust,ignore
#[serde(rename_all = "camelCase")]
pub struct SoftDelete { pub deleted_at: Option<DateTime<Utc>> }
impl SoftDelete { fn is_deleted(&self) -> bool; fn mark_deleted(&mut self, at); fn restore(&mut self); }

pub struct SoftDeletePolicy { /* guards "deleted_at" by default */ }
impl SoftDeletePolicy {
    pub fn new() -> Self;
    pub fn for_column(column) -> Self;
    pub fn predicate(&self) -> Predicate;                // "deleted_at" IS NULL
    pub fn apply(&self, filter: Filter) -> Filter;       // injects the guard up front (idempotent)
    pub fn apply_spec(&self, spec: Specification) -> Specification;
}
```

`apply` prepends a `deleted_at IS NULL` guard to a `Filter`, so
`WHERE "deleted_at" IS NULL AND <user predicates>` — the read-path
exclusion pyfly threads through `SoftDeleteRepository`. `apply_spec`
ANDs the same guard onto a `Specification`, keeping any OR sub-tree
grouped.

### `RoutingPolicy` + `read_only` + `NamedDataSources`

```rust,ignore
pub fn read_only() -> ReadOnlyGuard;   // RAII; restores prior state on drop (nesting-safe)
pub fn is_read_only() -> bool;

pub struct RoutingPolicy<S> { /* primary + optional replica factory */ }
impl<S> RoutingPolicy<S> {
    pub fn primary_only(primary) -> Self;
    pub fn with_replica(primary, replica) -> Self;
    pub fn has_replica(&self) -> bool;
    pub fn primary(&self) -> S;
    pub fn replica(&self) -> S;     // falls back to primary when none configured
    pub fn route(&self) -> S;       // replica inside a read_only() scope, else primary
}

pub struct NamedDataSources<S> { /* sorted name -> factory */ }
impl<S> NamedDataSources<S> {
    pub fn register(self, name, factory) -> Self;
    pub fn get(&self, name) -> Result<&S, RoutingError>;
    pub fn names(&self) -> Vec<String>;     // sorted
    pub fn contains(&self, name) -> bool;
    pub fn len(&self) -> usize;
}
```

`RoutingPolicy` is Spring's `AbstractRoutingDataSource`: it routes to a
read-replica inside a `read_only()` scope (when one is configured) and
to the primary otherwise. `read_only()` returns an RAII guard backed by
a thread-local flag — the Rust idiom for pyfly's `contextvar`-based
context manager; nesting restores the outer scope's state. The factory
type `S` is generic, so the crate stays free of any SQL driver.
`NamedDataSources` is the registry of additional named datasources.

## Testing

```bash
cargo test -p firefly-data
```

Covers `Page` total-pages math and JSON wire shape, every `Op`
rendering correctly, predicate ↔ argument index mapping with `IsNil`
skipping arg slots, and the in-memory repository's CRUD + paging —
plus Rust-specific object-safety, `Send + Sync`, and serde round-trip
checks.
