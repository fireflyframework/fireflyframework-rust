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

## Testing

```bash
cargo test -p firefly-data
```

Covers `Page` total-pages math and JSON wire shape, every `Op`
rendering correctly, predicate ↔ argument index mapping with `IsNil`
skipping arg slots, and the in-memory repository's CRUD + paging —
plus Rust-specific object-safety, `Send + Sync`, and serde round-trip
checks.
