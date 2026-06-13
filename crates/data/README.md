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

### Dialects & document lowering

`firefly-data` is a **technology-agnostic PORT crate**: the `Filter` /
`Specification` tree is the single source of truth, and it lowers to
**three** backends — relational SQL (any vendor), an in-memory matcher,
*and* a MongoDB-style document filter. Relational and document adapters
plug in on top of these without re-walking or re-implementing the tree.

#### `SqlDialect` — one tree, every relational vendor

```rust,ignore
pub trait SqlDialect {
    fn placeholder(&self, n: usize) -> String;                       // "$1" | "?"
    fn quote_ident(&self, id: &str) -> String;                       // "id"  | `id`
    fn render_in(&self, field: &str, start: usize, n_args: usize) -> String; // = ANY($n) | IN (?,…)
    fn ilike(&self, field_q: &str, ph: &str) -> String;              // ILIKE | LOWER(..) LIKE LOWER(..)
    // + in_arg_count() / expands_in_list() — how many arg slots IN consumes
}

// zero-sized vendor impls
pub struct PostgresDialect;   pub struct MySqlDialect;   pub struct SqliteDialect;

impl Filter {
    pub fn to_sql(&self) -> (String, Vec<Value>);                    // Postgres default
    pub fn to_sql_with(&self, d: &dyn SqlDialect) -> (String, Vec<Value>);
}
impl Specification {
    pub fn to_sql(&self) -> (String, Vec<Value>);                    // Postgres default
    pub fn to_sql_with(&self, d: &dyn SqlDialect) -> (String, Vec<Value>);
}
```

The DSL no longer hard-codes PostgreSQL syntax. The four vendor-specific
rendering decisions live behind the `SqlDialect` trait:

| concern | `PostgresDialect` | `MySqlDialect` | `SqliteDialect` |
|---|---|---|---|
| placeholder | `$1`, `$2`, … | `?` | `?` |
| identifier quote | `"ident"` | `` `ident` `` | `"ident"` |
| `IN` list | `= ANY($n)` (one array param) | `IN (?, ?, …)` | `IN (?, ?, …)` |
| case-insensitive `LIKE` | `field ILIKE $n` | `LOWER(field) LIKE LOWER(?)` | `LOWER(field) LIKE LOWER(?)` |

`to_sql()` / `Specification::to_sql()` are **unchanged** — they are
exactly `to_sql_with(&PostgresDialect)`, so existing callers and tests
keep working. A relational adapter (e.g. `firefly-data-sqlx`) just picks
the dialect at runtime from the backend kind and calls `to_sql_with`.

The returned **args vector always matches the placeholders the dialect
emitted**: Postgres binds an `IN` list as one array argument, while
MySQL/SQLite flatten it into one scalar argument per element, and the
placeholder numbering after an `IN` advances accordingly.

```rust
use firefly_data::{Filter, MySqlDialect, Op, Predicate, PostgresDialect, SqliteDialect};
use serde_json::json;

let f = Filter::new()
    .add(Predicate::new("role", Op::In, json!(["a", "b"])))
    .where_eq("active", true);

// Postgres: array param, `active` is $2
assert_eq!(
    f.to_sql_with(&PostgresDialect),
    (r#" WHERE "role" = ANY($1) AND "active" = $2"#.to_string(),
     vec![json!(["a", "b"]), json!(true)]),
);
// MySQL: expanded IN, args flattened
assert_eq!(
    f.to_sql_with(&MySqlDialect),
    (" WHERE `role` IN (?, ?) AND `active` = ?".to_string(),
     vec![json!("a"), json!("b"), json!(true)]),
);
// SQLite: like MySQL but ANSI-quoted
assert_eq!(f.to_sql_with(&SqliteDialect).0, r#" WHERE "role" IN (?, ?) AND "active" = ?"#);
```

#### Document lowering — `to_mongo()`

```rust,ignore
impl Filter        { pub fn to_mongo(&self) -> Value;  pub fn mongo_sort(&self) -> Value; }
impl Specification { pub fn to_mongo(&self) -> Value; }
impl ParsedQuery   { pub fn to_mongo(&self, args: &[Value]) -> Result<Value, QueryBindError>;
                     pub fn mongo_sort(&self) -> Value; }
```

`to_mongo()` lowers the **same** tree to a MongoDB `$`-operator filter
document, so a document adapter (e.g. `firefly-data-mongodb`) reuses the
`Specification` tree instead of re-walking it — mirroring pyfly's
`MongoSpecification` / `MongoQueryMethodCompiler`:

| spec node / op | Mongo |
|---|---|
| `eq` | `{field: {"$eq": v}}` |
| `ne` | `{field: {"$ne": v}}` |
| `gt` / `lt` / `gte` / `lte` | `{field: {"$gt": v}}` … |
| `like` / `ilike` | `{field: {"$regex": …}}` (`%`→`.*`, `_`→`.`; `ilike` adds `"$options":"i"`) |
| `in` | `{field: {"$in": v}}` |
| `isnil` | `{field: {"$eq": null}}` |
| `And([..])` | `{"$and": [..]}` |
| `Or([..])` | `{"$or": [..]}` |
| `Not(x)` | `{"$nor": [x]}` |
| `All` | `{}` (matches everything) |

```rust
use firefly_data::Specification;
use serde_json::json;

let spec = (Specification::eq("role", "admin") & Specification::eq("active", true))
    | Specification::eq("name", "Diana");

assert_eq!(
    spec.to_mongo(),
    json!({ "$or": [
        { "$and": [ { "role": { "$eq": "admin" } }, { "active": { "$eq": true } } ] },
        { "name": { "$eq": "Diana" } },
    ] }),
);
```

Sorting / paging are not part of the filter document (a Mongo cursor
applies those separately); `Filter::mongo_sort()` /
`ParsedQuery::mongo_sort()` return the cursor sort spec
(`{field: 1 | -1}`).

#### `UserProvider` — implicit current-user for auditing adapters

```rust,ignore
pub type UserProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

impl Auditor {
    pub fn with_user_provider(provider: UserProvider) -> Self;
    pub fn with_provider_and_clock(provider: UserProvider, clock) -> Self;
    pub fn current_user(&self) -> Option<String>;
    pub fn stamp_insert(&self, stamps: &mut AuditStamps); // resolves user from provider
    pub fn stamp_update(&self, stamps: &mut AuditStamps);
}
```

An adapter holds a `UserProvider` (wired from the request-context /
security crate) and an `Auditor`/`SoftDeletePolicy`, then auto-applies
them on every write/read path without the caller threading the user
through each call — the Rust analogue of pyfly's implicit
`RequestContext.current()` user lookup. The explicit
`on_insert(stamps, Some("alice"))` / `apply(filter)` helpers are
unchanged; the provider form is purely additive.

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

### `Mapper` (runtime object-to-object mapper / MapStruct equivalent)

```rust,ignore
pub struct Mapper { /* registered mappings + projections, keyed by (S, D) */ }

impl Mapper {
    pub fn new() -> Self;
    pub fn add_mapping<S, D>(&mut self, mapping: Mapping);            // custom rename/transform/exclude
    pub fn register_projection<S, D>(&mut self, projection: Projection);
    pub fn map<S: Serialize, D: DeserializeOwned>(&self, source: &S) -> Result<D, MapError>;
    pub fn map_list<S, D>(&self, sources: &[S]) -> Result<Vec<D>, MapError>;
    pub fn project<S, D>(&self, source: &S) -> Result<D, MapError>;
}

// fluent config builders
let mapping = Mapping::new()
    .rename("username", "name")                                      // source -> dest
    .transform("name", |v| json!(v.as_str().unwrap().to_uppercase())) // dest-keyed
    .exclude("is_active");                                           // keep dest default
let projection = Projection::new()
    .computed("total", |src| json!(src["quantity"].as_f64().unwrap() * src["unit_price"].as_f64().unwrap()));
```

The Rust port of pyfly's `data.mapper`. Rust has no runtime field
reflection, so the mapper bridges through `serde_json`: the source is
serialised to a JSON object, renames / exclusions / transformers are
applied, and the result is deserialised into the destination type — so
**nested models and collections of models recurse automatically** via
serde. `map` does name-matched conversion (with optional `Mapping`
config); `map_list` maps a slice; `project` maps onto a (usually
smaller) projection type with optional `Projection::computed` fields
that receive the whole source. Field renaming is **source → destination**
(pyfly's `field_map`); transformers and computed fields are keyed by
**destination** name.

### `Pageable` + `RequestSort` + `Order` (pagination request types)

```rust,ignore
pub struct Order { pub property: String, pub direction: Direction }
impl Order { fn asc(p) -> Self; fn desc(p) -> Self; }

pub struct RequestSort { pub orders: Vec<Order> }
impl RequestSort {
    fn by(props) -> Self;                  // ascending sort by properties
    fn unsorted() -> Self;
    fn and_then(self, &RequestSort) -> Self;
    fn ascending(self) -> Self;            // flip all to asc
    fn descending(self) -> Self;           // flip all to desc
    fn to_sorts(&self) -> Vec<Sort>;       // lower to the filter DSL
}

pub struct Pageable { pub page: usize, pub size: usize, pub sort: RequestSort } // page is 1-based
impl Pageable {
    fn of(page, size, sort) -> Result<Self, PageableError>; // validates page>=1, size>=1
    fn paged(page, size) -> Result<Self, PageableError>;    // unsorted
    fn unpaged() -> Self;                                   // size == UNPAGED_SIZE
    fn is_paged(&self) -> bool;
    fn offset(&self) -> usize;                              // (page-1)*size
    fn next(&self) -> Pageable;
    fn previous(&self) -> Pageable;                         // min page 1
    fn to_filter(&self) -> Filter;                          // 1-based page -> 0-based filter page
    fn apply_to(&self, filter: Filter) -> Filter;           // keep predicates, set paging+sort
}
```

The Rust port of pyfly's `data.pageable`. These are the **request**
side of paging (what the caller asks for), distinct from the `Page<T>`
**response** envelope. The page number is **1-based** with `page >= 1`
validation (`PageableError`); `to_filter` translates it to the filter
DSL's **0-based** page index. The sort collection is named `RequestSort`
(not `Sort`) to avoid colliding with the SQL-render `Sort` already
exported by the filter DSL. Paging is wired into the repository contract
via `Repository::find_page(&self, &Pageable)`, a default method lowering
through `to_filter` to `find`.

### `QueryMethodParser` (Spring-Data derived query methods)

```rust,ignore
pub struct QueryMethodParser;
impl QueryMethodParser {
    pub fn parse(&self, method_name: &str) -> Result<ParsedQuery, QueryParseError>;
}

pub struct ParsedQuery {
    pub prefix: QueryPrefix,                 // Find | Count | Exists | Delete
    pub predicates: Vec<FieldPredicate>,     // field + QueryOperator
    pub connectors: Vec<String>,             // "and" | "or"
    pub order_clauses: Vec<OrderClause>,
}
impl ParsedQuery {
    pub fn arg_count(&self) -> usize;
    pub fn to_specification(&self, args: &[Value]) -> Result<Specification, QueryBindError>;
    pub fn to_filter(&self, args: &[Value]) -> Result<Option<Filter>, QueryBindError>; // None for OR
    pub fn to_mongo(&self, args: &[Value]) -> Result<Value, QueryBindError>;           // $-operator filter
    pub fn to_sql(&self, dialect: &dyn SqlDialect, table: &str, args: &[Value])
        -> Result<DerivedSql, QueryBindError>;  // complete per-prefix statement
    pub fn evaluate<'a, T: Serialize>(&self, entities: &'a [T], args: &[Value]) -> Result<Vec<&'a T>, QueryBindError>;
    pub fn count<T: Serialize>(&self, entities: &[T], args: &[Value]) -> Result<usize, QueryBindError>;
    pub fn exists<T: Serialize>(&self, entities: &[T], args: &[Value]) -> Result<bool, QueryBindError>;
}
```

The Rust port of pyfly's `data.query_parser`. Parses
`find_by_status_and_role_order_by_name_desc`-style method names into a
structured `ParsedQuery` (prefixes `find_by`/`count_by`/`exists_by`/
`delete_by`, connectors `_and_`/`_or_`, operator suffixes checked
longest-first so `_greater_than_equal` beats `_greater_than`, and
chainable `_order_by_…` clauses). A `ParsedQuery` lowers — **with bound
argument values** — to the existing `Specification` tree
(`to_specification`) or a flat `Filter` (`to_filter`, `None` when the
query contains an `OR` the AND-only filter cannot represent), and
executes against any in-memory `serde`-serialisable collection via
`evaluate` / `count` / `exists`. `_between` lowers to two predicates
(`>=` and `<=`), `_containing` to `LIKE %value%`, `_is_not_null` to
`NOT (… IS NULL)`.

For **end-to-end** execution against a database, `to_sql` renders a
complete dialect-aware statement per prefix — `SELECT * … [ORDER BY …]`
(find), `SELECT COUNT(*) …` (count), `SELECT CASE WHEN EXISTS(…) THEN 1
ELSE 0 END` (exists), `DELETE …` (delete) — and `to_mongo` lowers to a
`$`-operator filter document. The `firefly-data-sqlx` and
`firefly-data-mongodb` adapters call these from their
`find_by_derived` / `count_by_derived` / `exists_by_derived` /
`delete_by_derived` helpers, so a `find_by_email` method name runs as a
real query (the Rust analogue of pyfly's repository bean
post-processor).

### `CustomQuery` (the `@query` custom-query path)

```rust,ignore
pub struct CustomQuery { /* value + native flag */ }
impl CustomQuery {
    pub fn native(value: impl Into<String>) -> Self;  // raw SQL
    pub fn jpql(value: impl Into<String>) -> Self;     // JPQL-like, transpiled
    pub fn bind(&self, dialect: &dyn SqlDialect, entity_name: &str, table: &str,
                params: &BTreeMap<String, Value>) -> Result<BoundQuery, CustomQueryError>;
}
pub struct BoundQuery { pub sql: String, pub args: Vec<Value>, pub shape: QueryShape }
pub enum QueryShape { Count, Exists, List }  // inferred from the statement

pub fn transpile_jpql(jpql: &str, entity_name: &str, table: &str) -> String;
pub fn substitute_named_params(value: &Value, params: &BTreeMap<String, Value>) -> Value;
```

The Rust port of pyfly's `@query` decorator + `QueryExecutor` /
`MongoQueryExecutor`. A relational `CustomQuery` carries raw SQL
(`native`) or a JPQL-like string (`jpql`, transpiled by `transpile_jpql`
— `FROM Entity alias` → `FROM table`, `SELECT alias`/`COUNT(alias)` →
`SELECT *`/`COUNT(*)`, `alias.field` → `field`, `= true`/`= false` →
`= 1`/`= 0`); `bind` rewrites every `:param` to the dialect's positional
placeholder and infers the return `QueryShape`. For document stores,
`substitute_named_params` substitutes `":param"` placeholders in a parsed
JSON filter / aggregation pipeline (typed for an exact `":param"`,
stringified for an embedded one). The adapters execute these via
`query_list` / `query_count` / `query_exists` / `query_execute` (sqlx)
and `query_find` / `query_aggregate` (mongodb).

### `ColumnProjection` (DB-level interface projections)

```rust,ignore
pub struct ColumnProjection { /* name + ordered columns */ }
impl ColumnProjection {
    pub fn new(name: impl Into<String>, columns: impl IntoIterator<Item = impl Into<String>>) -> Self;
    pub fn columns(&self) -> &[String];
    pub fn select_list(&self, quote: impl Fn(&str) -> String) -> String;  // SQL SELECT list
    pub fn to_mongo(&self) -> Value;                                       // {col: 1, …}
    pub fn project<T: Serialize>(&self, entity: &T) -> Value;              // in-memory narrow
}
```

The DB-side complement of `Mapper::project`: where the mapper projects an
*already-fetched* full entity object-to-object, a `ColumnProjection`
narrows the `SELECT` list (or Mongo projection document) so **only** the
projected columns cross the wire — the missing half of pyfly's
projection-aware query compiler. The adapters run it via
`project_by_spec`.

## Reactive

On top of the blocking `async fn` `Repository` contract, this crate adds
a **reactive** CRUD surface — the Spring Data **R2DBC** analog — built on
the [`firefly-reactive`](../reactive) crate (Rust's Project Reactor /
WebFlux `Mono` / `Flux`). It is purely **additive**: nothing about the
existing `Repository` / `MemoryRepository` API changes; the reactive
types sit alongside.

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

A "no row" `find_by_id` maps to an **empty** `Mono` (Reactor's
`Mono.empty()`), exactly as Spring Data signals a missing `findById`.

### In-memory (`ReactiveMemoryRepository`)

The reactive twin of `MemoryRepository`, for tests. Drive the publishers
with `block()` / `collect_list()` (Reactor's `block()`):

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

### Real Postgres (`PostgresReactiveRepository`), streaming rows as a `Flux`

The production repository over `tokio-postgres`. Reads drive the driver's
`query_raw` **row stream**, so each row is decoded by a `RowMapper` and
emitted the moment it arrives over the wire — a million-row table never
lands fully in memory (no collect-then-emit). Writes use a per-entity
`inserter` closure that renders a `T` to an upsert `(sql, params)` whose
`RETURNING` projects exactly the configured columns:

```rust
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
`RowMapper`.

### Reactive specification / paging

`ReactiveSpecificationRepository` (implemented by `ReactiveMemoryRepository`)
runs a composable `Specification` predicate with an optional `Pageable`
window and **streams** the matches as a `Flux` — the reactive analog of
`findAll(Specification, Pageable)`, but with no intermediate `Page<T>`
envelope, so it plugs straight into an NDJSON / SSE endpoint with
backpressure.

## Testing

```bash
cargo test -p firefly-data
```

Covers `Page` total-pages math and JSON wire shape, every `Op`
rendering correctly, predicate ↔ argument index mapping with `IsNil`
skipping arg slots, and the in-memory repository's CRUD + paging —
plus Rust-specific object-safety, `Send + Sync`, and serde round-trip
checks.

The reactive surface adds full-CRUD coverage of
`ReactiveMemoryRepository` (driven via `block` / `collect_list`) and the
streaming specification/paging query. The `PostgresReactiveRepository`
round-trip is `#[ignore = "requires postgres"]` and reads
`DATABASE_URL` / `POSTGRES_URL`:

```bash
DATABASE_URL=postgres://localhost/app cargo test -p firefly-data -- --ignored
```
