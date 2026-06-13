# `firefly-data-mongodb`

> **Tier:** Platform · **Status:** Full · **pyfly original:** `pyfly.data.document.mongodb`

## Overview

`firefly-data-mongodb` is the **document** persistence adapter: it
implements the [`firefly-data`](../data) repository ports over the
official [`mongodb`](https://crates.io/crates/mongodb) crate, behind the
**same** reactive `Repository` / `ReactiveCrudRepository` surface as the
relational adapters. A service can swap a Postgres repository for a Mongo
one without touching its call sites — that is the whole point of the
hexagonal split.

The [`Specification`](../data) tree is the single source of truth for
queries: `Specification::to_mongo()` lowers it to a MongoDB
`$`-operator filter document exactly as `to_sql()` lowers it for
relational stores, so the *same* spec drives SQL, in-memory matching, and
MongoDB.

This is the Rust port of pyfly's `MongoRepository`, `MongoSpecification`,
and `BaseDocument`.

## Public surface

### `MongoRepository<T, ID>`

A generic CRUD + specification + paging repository over a `mongodb`
collection, where `T: Serialize + DeserializeOwned`. It implements both
firefly-data reactive ports:

- **`ReactiveCrudRepository<T, ID>`** — `find_all`, `find_all_by_id`,
  `find_by_id`, `exists_by_id`, `save`, `save_all`, `delete_by_id`,
  `delete_all`, `count`.
- **`ReactiveSpecificationRepository<T>`** — `find_by_spec`,
  `find_by_spec_paged` (consuming `Specification::to_mongo`, with
  `sort` / `skip` / `limit` derived from a `Pageable`).

Plus, beyond the shared ports:

- `find_page(spec, pageable) -> Mono<Page<T>>` — the canonical
  `Page<T>` envelope (content + total + page metadata).
- `save_audited` / `save_all_audited` — upsert with automatic audit
  stamping (see `Audited` below).
- `with_auditor(Auditor)` — wire automatic audit stamping on writes.
- `with_soft_delete(SoftDeletePolicy)` — wire automatic soft-delete
  filtering on reads and turn `delete_by_id` into a logical delete.
- **Derived & custom queries executed end-to-end** — the document analogue
  of pyfly's repository bean post-processor:
  - `find_by_derived` / `count_by_derived` / `exists_by_derived` /
    `delete_by_derived` parse a `find_by_status_and_role`-style method name
    and lower it (through the shared `Specification` tree) to a
    `$`-operator filter, executed against the collection.
  - `query_find(filter_json, params)` runs a `@query` JSON filter document
    with `":param"` substitution; `query_aggregate(pipeline_json, params)`
    runs a `@query` aggregation pipeline (results stream as
    `serde_json::Value`).
  - `project_by_spec(projection, spec)` applies a DB-level
    `ColumnProjection` (a Mongo projection document) so only the projected
    fields are returned.

Reads **stream lazily** off the driver's cursor as a `Flux<T>`; nothing
is buffered before the first row.

```rust,ignore
use firefly_data::{Op, Predicate, ReactiveCrudRepository, ReactiveSpecificationRepository, Specification};
use firefly_data_mongodb::{BaseDocument, MongoRepository};
use mongodb::bson::{Bson, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct UserDocument {
    #[serde(rename = "_id")]
    id: String,
    name: String,
    #[serde(flatten)]
    base: BaseDocument,
}

let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
let collection = client.database("app").collection::<Document>("users");

let repo: MongoRepository<UserDocument, String> =
    MongoRepository::new(collection, |u: &UserDocument| Bson::String(u.id.clone()));

repo.save(UserDocument { id: "u1".into(), name: "alice".into(), base: BaseDocument::new() })
    .block()
    .await?;

// The SAME Specification tree that drives SQL drives Mongo here.
let spec = Specification::pred(Predicate::new("name", Op::Eq, "alice"));
let hits = repo.find_by_spec(spec).collect_list().block().await?;
```

### `BaseDocument`

The audit-stamp + soft-delete mixin every document embeds with
`#[serde(flatten)]` — the Rust analogue of pyfly's `BaseDocument`. Its
fields surface at the document's top level (`createdAt`, `updatedAt`,
`createdBy`, `updatedBy`, `deletedAt`). Stamping is delegated to
firefly-data's `Auditor` and `SoftDelete`, so audit / soft-delete
semantics match the relational adapter exactly.

```rust,ignore
use firefly_data::Auditor;
use firefly_data_mongodb::BaseDocument;

let auditor = Auditor::new();
let mut base = BaseDocument::new();
base.stamp_insert(&auditor);   // sets created_at / updated_at (+ *_by if a UserProvider is wired)
assert!(base.audit.created_at.is_some());
assert!(!base.is_deleted());
```

### `Audited`

The hook by which a document exposes its embedded `BaseDocument` so the
repository can auto-stamp audit fields on write. Implement it for any
entity that embeds a `BaseDocument`, then use
`MongoRepository::save_audited` / `save_all_audited`:

```rust,ignore
use firefly_data_mongodb::{Audited, BaseDocument};

impl Audited for UserDocument {
    fn base_mut(&mut self) -> &mut BaseDocument { &mut self.base }
}
```

## Specification → MongoDB lowering

| `Specification` node | MongoDB filter |
|----------------------|----------------|
| `All`                | `{}` (matches everything) |
| `Pred(field Eq v)`   | `{field: {$eq: v}}` |
| `Pred(field Gt v)`   | `{field: {$gt: v}}` (and `$lt` / `$gte` / `$lte` / `$ne`) |
| `Pred(field In [..])`| `{field: {$in: [..]}}` |
| `Pred(field Like p)` | `{field: {$regex: ...}}` (SQL `%`/`_` → regex) |
| `And([..])`          | `{$and: [..]}` |
| `Or([..])`           | `{$or: [..]}` |
| `Not(s)`             | `{$nor: [s]}` |

The lowering lives in `firefly-data` (`Specification::to_mongo`), so the
tree stays the single source of truth.

## Soft delete

When a `SoftDeletePolicy` is wired with `with_soft_delete`, **every read
path** (`find_all`, `find_all_by_id`, `find_by_id`, `exists_by_id`,
`count`, `find_by_spec`, `find_by_spec_paged`, `find_page`) injects a
`{"<column>": null}` guard so logically deleted documents stay hidden,
and `delete_by_id` becomes a `$set` of the stamp column rather than a
physical removal. `delete_all` always removes physically (Spring Data
`deleteAll` parity).

## Testing

Pure-shape unit tests (filter / options / soft-delete guard rendering,
audit stamping, serde wire shape) run with no infrastructure. The full
round-trip integration test (`mongodb_round_trip`) is **env-gated**: it
runs only when `FIREFLY_TEST_MONGODB_URL` (fallback `MONGODB_URL`) is
set, and skips cleanly otherwise so `cargo test` stays green on a bare
machine.

```bash
# Offline: the round-trip tests skip, everything else runs.
cargo test -p firefly-data-mongodb

# Against a live mongod (also exercises the derived/custom/projection paths):
FIREFLY_TEST_MONGODB_URL=mongodb://localhost:27017 \
  cargo test -p firefly-data-mongodb
```

## Actuator integration (feature `actuator`)

Enable the `actuator` feature for a database health component, the Rust port
of pyfly's database health probe:

```toml
firefly-data-mongodb = { version = "26.6.3", features = ["actuator"] }
```

`MongoHealthIndicator` implements `firefly_actuator::HealthIndicator`: it
issues the server `ping` command and reports `UP` (with the database name on
`details.database`) — the `db` component on `GET /actuator/health`.
`MongoHealthIndicator::named(db, "db-reporting")` probes a named database
under its own component name.

## License

Apache-2.0. Copyright 2026 Firefly Software Foundation.
