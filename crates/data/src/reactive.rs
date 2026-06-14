// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The **reactive** CRUD surface — the Spring Data **R2DBC** analog.
//!
//! Where [`Repository`](crate::Repository) is the blocking-style
//! `async fn` contract (one awaited value per call), this module adds a
//! *streaming* contract built on [`firefly_reactive`]'s [`Mono`] / [`Flux`]
//! — the Rust port of Project Reactor and the engine behind Spring
//! WebFlux. It is the exact analog of Spring Data's
//! `ReactiveCrudRepository<T, ID>`:
//!
//! | Spring Data R2DBC                              | firefly-data reactive                              |
//! |-----------------------------------------------|----------------------------------------------------|
//! | `ReactiveCrudRepository<T, ID>`               | [`ReactiveCrudRepository<T, ID>`]                  |
//! | `Flux<T> findAll()`                           | [`find_all`](ReactiveCrudRepository::find_all)     |
//! | `Flux<T> findAllById(ids)`                    | [`find_all_by_id`](ReactiveCrudRepository::find_all_by_id) |
//! | `Mono<T> findById(id)`                        | [`find_by_id`](ReactiveCrudRepository::find_by_id) |
//! | `Mono<Boolean> existsById(id)`                | [`exists_by_id`](ReactiveCrudRepository::exists_by_id) |
//! | `Mono<T> save(e)`                             | [`save`](ReactiveCrudRepository::save)             |
//! | `Flux<T> saveAll(es)`                         | [`save_all`](ReactiveCrudRepository::save_all)     |
//! | `Mono<Void> deleteById(id)`                   | [`delete_by_id`](ReactiveCrudRepository::delete_by_id) |
//! | `Mono<Void> deleteAll()`                      | [`delete_all`](ReactiveCrudRepository::delete_all) |
//! | `Mono<Long> count()`                          | [`count`](ReactiveCrudRepository::count)           |
//!
//! This surface is **purely additive**: it sits alongside the existing
//! [`Repository`](crate::Repository) without changing any of its
//! signatures. The in-memory [`ReactiveMemoryRepository`] mirrors
//! [`MemoryRepository`](crate::MemoryRepository) for tests, and
//! [`PostgresReactiveRepository`] is a real `tokio-postgres` repository
//! that **streams rows lazily** as a [`Flux`] (it drives the driver's
//! `query_raw` row stream / a portal — there is no collect-then-emit
//! buffering of the result set).
//!
//! [`Mono`]: firefly_reactive::Mono
//! [`Flux`]: firefly_reactive::Flux

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use firefly_kernel::FireflyError;
use firefly_reactive::{Flux, Mono};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Row};

/// The generic **reactive** typed CRUD contract — the Rust port of
/// Spring Data's `ReactiveCrudRepository<T, ID>`.
///
/// Every method returns a [`firefly_reactive`] publisher
/// ([`Mono`](firefly_reactive::Mono) for at-most-one,
/// [`Flux`](firefly_reactive::Flux) for a stream) rather than an awaited
/// value, so callers compose them with the full Reactor operator set and
/// `Flux` results stream straight out of an axum handler (NDJSON / SSE)
/// without ever materialising the whole result set in memory.
///
/// The blocking [`Repository`](crate::Repository) and this reactive
/// contract coexist: a store may implement one, the other, or both. The
/// errors surface through the publishers' fixed
/// [`FireflyError`](firefly_kernel::FireflyError) channel; "no row" maps
/// to an empty [`Mono`] (Reactor's `Mono.empty()`), exactly as Spring
/// Data signals a missing `findById`.
#[async_trait]
pub trait ReactiveCrudRepository<T, ID>: Send + Sync
where
    T: Send + 'static,
    ID: Send + Sync + 'static,
{
    /// Streams **every** entity. Spring Data's `Flux<T> findAll()`.
    ///
    /// The returned [`Flux`] is lazy: nothing is fetched until it is
    /// subscribed/collected, and a database-backed implementation streams
    /// rows as they arrive rather than buffering the whole table.
    fn find_all(&self) -> Flux<T>;

    /// Streams the entities whose ids are in `ids`. Spring Data's
    /// `Flux<T> findAllById(Iterable<ID>)`. Missing ids are simply absent
    /// from the stream (no error).
    fn find_all_by_id(&self, ids: Vec<ID>) -> Flux<T>;

    /// Looks up a single entity by primary key. Spring Data's
    /// `Mono<T> findById(ID)` — a present value on a hit, an **empty**
    /// [`Mono`](firefly_reactive::Mono) (`Mono.empty()`) on a miss.
    fn find_by_id(&self, id: ID) -> Mono<T>;

    /// Reports whether an entity with `id` exists. Spring Data's
    /// `Mono<Boolean> existsById(ID)`.
    fn exists_by_id(&self, id: ID) -> Mono<bool>;

    /// Inserts or updates `entity` (upsert by id) and emits the persisted
    /// value. Spring Data's `Mono<S> save(S)`.
    fn save(&self, entity: T) -> Mono<T>;

    /// Saves every entity in `entities`, emitting each persisted value.
    /// Spring Data's `Flux<S> saveAll(Iterable<S>)`.
    fn save_all(&self, entities: Vec<T>) -> Flux<T>;

    /// Removes the entity with `id`. Deleting a missing id is **not** an
    /// error. Spring Data's `Mono<Void> deleteById(ID)` — the unit value
    /// stands in for `Void`.
    fn delete_by_id(&self, id: ID) -> Mono<()>;

    /// Removes every entity. Spring Data's `Mono<Void> deleteAll()`.
    fn delete_all(&self) -> Mono<()>;

    /// Counts all entities. Spring Data's `Mono<Long> count()`.
    fn count(&self) -> Mono<u64>;
}

/// An in-process [`ReactiveCrudRepository`] backed by a map — the
/// reactive twin of [`MemoryRepository`](crate::MemoryRepository), and
/// the analog of Spring's in-memory `ReactiveCrudRepository` test double.
///
/// Id extraction is delegated to a user-supplied keyer closure. The store
/// is shared behind an [`Arc`] + [`RwLock`], so the publishers returned
/// by each method are `Send + 'static` and can be subscribed on any
/// scheduler. Because the data lives in memory, every method's `Flux` /
/// `Mono` is still lazy (it reads the map at subscription time), matching
/// Reactor's "nothing happens until you subscribe" contract.
pub struct ReactiveMemoryRepository<T, ID> {
    keyer: Arc<dyn Fn(&T) -> ID + Send + Sync>,
    store: Arc<RwLock<HashMap<ID, T>>>,
}

impl<T, ID> ReactiveMemoryRepository<T, ID>
where
    ID: Eq + Hash,
{
    /// Returns an empty repository whose ids are derived by `keyer`.
    pub fn new(keyer: impl Fn(&T) -> ID + Send + Sync + 'static) -> Self {
        ReactiveMemoryRepository {
            keyer: Arc::new(keyer),
            store: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl<T, ID> Clone for ReactiveMemoryRepository<T, ID> {
    fn clone(&self) -> Self {
        ReactiveMemoryRepository {
            keyer: Arc::clone(&self.keyer),
            store: Arc::clone(&self.store),
        }
    }
}

#[async_trait]
impl<T, ID> ReactiveCrudRepository<T, ID> for ReactiveMemoryRepository<T, ID>
where
    T: Clone + Send + Sync + 'static,
    ID: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn find_all(&self) -> Flux<T> {
        let store = Arc::clone(&self.store);
        Flux::defer(move || {
            let snapshot: Vec<T> = {
                let guard = store.read().expect("data: store lock poisoned");
                guard.values().cloned().collect()
            };
            Flux::from_iter(snapshot)
        })
    }

    fn find_all_by_id(&self, ids: Vec<ID>) -> Flux<T> {
        let store = Arc::clone(&self.store);
        Flux::defer(move || {
            let snapshot: Vec<T> = {
                let guard = store.read().expect("data: store lock poisoned");
                ids.iter().filter_map(|id| guard.get(id).cloned()).collect()
            };
            Flux::from_iter(snapshot)
        })
    }

    fn find_by_id(&self, id: ID) -> Mono<T> {
        let store = Arc::clone(&self.store);
        Mono::from_callable(move || {
            let guard = store.read().expect("data: store lock poisoned");
            Ok(guard.get(&id).cloned())
        })
    }

    fn exists_by_id(&self, id: ID) -> Mono<bool> {
        let store = Arc::clone(&self.store);
        Mono::from_callable(move || {
            let guard = store.read().expect("data: store lock poisoned");
            Ok(Some(guard.contains_key(&id)))
        })
    }

    fn save(&self, entity: T) -> Mono<T> {
        let store = Arc::clone(&self.store);
        let keyer = Arc::clone(&self.keyer);
        Mono::from_callable(move || {
            let key = keyer(&entity);
            let mut guard = store.write().expect("data: store lock poisoned");
            guard.insert(key, entity.clone());
            Ok(Some(entity))
        })
    }

    fn save_all(&self, entities: Vec<T>) -> Flux<T> {
        let store = Arc::clone(&self.store);
        let keyer = Arc::clone(&self.keyer);
        Flux::defer(move || {
            let mut guard = store.write().expect("data: store lock poisoned");
            for entity in &entities {
                guard.insert(keyer(entity), entity.clone());
            }
            drop(guard);
            Flux::from_iter(entities)
        })
    }

    fn delete_by_id(&self, id: ID) -> Mono<()> {
        let store = Arc::clone(&self.store);
        Mono::from_callable(move || {
            let mut guard = store.write().expect("data: store lock poisoned");
            guard.remove(&id);
            Ok(Some(()))
        })
    }

    fn delete_all(&self) -> Mono<()> {
        let store = Arc::clone(&self.store);
        Mono::from_callable(move || {
            let mut guard = store.write().expect("data: store lock poisoned");
            guard.clear();
            Ok(Some(()))
        })
    }

    fn count(&self) -> Mono<u64> {
        let store = Arc::clone(&self.store);
        Mono::from_callable(move || {
            let guard = store.read().expect("data: store lock poisoned");
            Ok(Some(guard.len() as u64))
        })
    }
}

/// A **reactive** specification/paging query surface — the analog of
/// Spring Data's reactive `JpaSpecificationExecutor` (and pyfly's
/// specification queries), returning a [`Flux`] rather than an awaited
/// page.
///
/// Where [`ReactiveCrudRepository`] gives you whole-table reads, this
/// trait runs a composable [`Specification`](crate::Specification)
/// predicate and an optional [`Pageable`](crate::Pageable) window,
/// **streaming** the matching entities. Because the result is a `Flux`, a
/// paged query plugs straight into an NDJSON / SSE endpoint with
/// backpressure — there is no intermediate `Page<T>` envelope to buffer.
#[async_trait]
pub trait ReactiveSpecificationRepository<T>: Send + Sync
where
    T: Send + 'static,
{
    /// Streams every entity matching `spec`. The reactive analog of
    /// `findAll(Specification)`.
    fn find_by_spec(&self, spec: crate::Specification) -> Flux<T>;

    /// Streams the page of entities matching `spec`, applying the
    /// `pageable`'s offset/limit window (sort is honoured by the in-memory
    /// implementation when the entity serialises to a JSON object). The
    /// reactive analog of `findAll(Specification, Pageable)` — but it
    /// returns the rows as a `Flux`, not a `Page`.
    fn find_by_spec_paged(&self, spec: crate::Specification, pageable: crate::Pageable) -> Flux<T>;
}

#[async_trait]
impl<T, ID> ReactiveSpecificationRepository<T> for ReactiveMemoryRepository<T, ID>
where
    T: Clone + Send + Sync + serde::Serialize + 'static,
    ID: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn find_by_spec(&self, spec: crate::Specification) -> Flux<T> {
        let store = Arc::clone(&self.store);
        Flux::defer(move || {
            let matched: Vec<T> = {
                let guard = store.read().expect("data: store lock poisoned");
                guard
                    .values()
                    .filter(|e| spec.matches(*e))
                    .cloned()
                    .collect()
            };
            Flux::from_iter(matched)
        })
    }

    fn find_by_spec_paged(&self, spec: crate::Specification, pageable: crate::Pageable) -> Flux<T> {
        let store = Arc::clone(&self.store);
        Flux::defer(move || {
            let mut matched: Vec<T> = {
                let guard = store.read().expect("data: store lock poisoned");
                guard
                    .values()
                    .filter(|e| spec.matches(*e))
                    .cloned()
                    .collect()
            };
            // Honour the pageable's sort (each entity is compared by its JSON
            // projection) before windowing — so `find_all_sorted` orders
            // correctly even on the in-memory repository.
            if pageable.sort.is_sorted() {
                let orders = pageable.sort.orders.clone();
                matched.sort_by(|a, b| {
                    let va = serde_json::to_value(a).unwrap_or(serde_json::Value::Null);
                    let vb = serde_json::to_value(b).unwrap_or(serde_json::Value::Null);
                    for o in &orders {
                        let fa = va.get(&o.property).unwrap_or(&serde_json::Value::Null);
                        let fb = vb.get(&o.property).unwrap_or(&serde_json::Value::Null);
                        let mut ord = compare_json_values(fa, fb);
                        if matches!(o.direction, crate::Direction::Desc) {
                            ord = ord.reverse();
                        }
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
            }
            let windowed = if pageable.is_paged() {
                let from = pageable.offset().min(matched.len());
                let to = (from + pageable.size).min(matched.len());
                matched[from..to].to_vec()
            } else {
                matched
            };
            Flux::from_iter(windowed)
        })
    }
}

/// Orders two JSON scalars for the in-memory sort: numbers numerically,
/// strings/bools lexically, with `null` sorting first. Mixed/!comparable types
/// fall back to `Equal` (stable order preserved).
fn compare_json_values(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    use serde_json::Value;
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&y.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// Spring Data's reactive **sorting + paging** repository — the
/// `ReactiveSortingRepository` / `PagingAndSortingRepository` analog for
/// WebFlux. It adds whole-table `findAll(Sort)` and `findAll(Pageable)` on top
/// of [`ReactiveCrudRepository`], and is implemented **for free** over any
/// repository that is also a [`ReactiveSpecificationRepository`] (the sort /
/// page is run as a match-all [`Specification`](crate::Specification)). So
/// every [`SqlxReactiveRepository`](crate) and
/// [`ReactiveMemoryRepository`] gains it automatically — no per-adapter code.
pub trait ReactiveSortingRepository<T, ID>:
    ReactiveCrudRepository<T, ID> + ReactiveSpecificationRepository<T>
where
    T: Send + 'static,
    ID: Send + Sync + 'static,
{
    /// Streams every entity in the given sort order — Spring's
    /// `Flux<T> findAll(Sort)`.
    fn find_all_sorted(&self, sort: crate::RequestSort) -> Flux<T> {
        let pageable = crate::Pageable {
            sort,
            ..crate::Pageable::unpaged()
        };
        self.find_by_spec_paged(crate::Specification::all(), pageable)
    }

    /// Streams the requested page (offset / limit + sort) — Spring's
    /// `PagingAndSortingRepository` `findAll(Pageable)`. WebFlux-style, the page
    /// is streamed as a `Flux` window rather than buffered into a `Page<T>`
    /// (use [`Page`](crate::Page) + a count query when you need the envelope).
    fn find_all_paged(&self, pageable: crate::Pageable) -> Flux<T> {
        self.find_by_spec_paged(crate::Specification::all(), pageable)
    }
}

/// Blanket impl: any reactive repository that is both a CRUD repository and a
/// specification repository is automatically a sorting/paging repository.
impl<R, T, ID> ReactiveSortingRepository<T, ID> for R
where
    R: ReactiveCrudRepository<T, ID> + ReactiveSpecificationRepository<T>,
    T: Send + 'static,
    ID: Send + Sync + 'static,
{
}

/// Maps a `tokio-postgres` [`Row`] into a domain entity `T`.
///
/// This is the reactive analog of Spring Data R2DBC's `BiFunction<Row,
/// RowMetadata, T>` row-mapping function. Implement it (or pass a
/// closure, since the trait is blanket-implemented for
/// `Fn(&Row) -> Result<T, FireflyError>`) to tell
/// [`PostgresReactiveRepository`] how to decode each streamed row.
///
/// A mapper must be `Send + Sync` because the streaming `Flux` may be
/// driven on any scheduler.
pub trait RowMapper<T>: Send + Sync {
    /// Decodes a single database row into a `T`, or fails the stream with
    /// a [`FireflyError`] (mapped to a 500 by the framework's RFC 7807
    /// layer).
    fn map_row(&self, row: &Row) -> Result<T, FireflyError>;
}

impl<T, F> RowMapper<T> for F
where
    F: Fn(&Row) -> Result<T, FireflyError> + Send + Sync,
{
    fn map_row(&self, row: &Row) -> Result<T, FireflyError> {
        self(row)
    }
}

/// The table/column configuration a [`PostgresReactiveRepository`] uses to
/// build its SQL. The analog of the metadata Spring Data derives from
/// `@Table` / `@Id` / `@Column` annotations.
///
/// Identifiers are emitted double-quoted, so mixed-case and reserved-word
/// table/column names are safe; values are always bound as `$n`
/// parameters (never string-interpolated).
#[derive(Debug, Clone)]
pub struct TableConfig {
    /// The table name (e.g. `"users"`).
    pub table: String,
    /// The primary-key column name (e.g. `"id"`).
    pub id_column: String,
    /// Every column to project, in order, for `SELECT` reads. The
    /// [`RowMapper`] must decode rows shaped by exactly these columns.
    pub columns: Vec<String>,
}

impl TableConfig {
    /// Builds a config from a table name, an id column, and the projected
    /// columns.
    pub fn new(
        table: impl Into<String>,
        id_column: impl Into<String>,
        columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        TableConfig {
            table: table.into(),
            id_column: id_column.into(),
            columns: columns.into_iter().map(Into::into).collect(),
        }
    }

    fn quoted_columns(&self) -> String {
        self.columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn select_all_sql(&self) -> String {
        format!("SELECT {} FROM \"{}\"", self.quoted_columns(), self.table)
    }
}

/// A **real** Postgres [`ReactiveCrudRepository`] over `tokio-postgres`
/// that **streams rows lazily** as a [`Flux`] — the production-grade
/// analog of Spring Data R2DBC's `R2dbcRepository`.
///
/// Reads ([`find_all`](ReactiveCrudRepository::find_all),
/// [`find_all_by_id`](ReactiveCrudRepository::find_all_by_id)) drive the
/// driver's [`query_raw`](tokio_postgres::Client::query_raw) **row
/// stream**: each row is decoded by the [`RowMapper`] and emitted the
/// moment it arrives over the wire, so a million-row table never lands
/// fully in memory. This is the reactive backpressured streaming Spring
/// Data R2DBC gives you over R2DBC's `Result.map(...)`.
///
/// Writes are deliberately **not** generic: a generic upsert cannot know
/// how to bind an arbitrary `T`'s columns without reflection. Supply an
/// `inserter` closure that turns a `T` into `(sql, params)` for an
/// `INSERT ... ON CONFLICT ... DO UPDATE ... RETURNING ...`; the repo runs
/// it and maps the returned row back through the [`RowMapper`]. This is
/// the same split Spring uses (derived reads, custom `@Query` writes) made
/// explicit. See the crate README's "Reactive" section for a worked
/// `User` example.
///
/// The id type is fixed to a single `ToSql + Sync` value `ID`, matching
/// the common single-column primary key.
pub struct PostgresReactiveRepository<T, ID> {
    client: Arc<Client>,
    config: TableConfig,
    mapper: Arc<dyn RowMapper<T>>,
    #[allow(clippy::type_complexity)]
    inserter: Arc<dyn Fn(&T) -> (String, Vec<Box<dyn ToSql + Sync + Send>>) + Send + Sync>,
    _id: std::marker::PhantomData<fn() -> ID>,
}

impl<T, ID> PostgresReactiveRepository<T, ID>
where
    T: Send + Sync + 'static,
    ID: ToSql + Sync + Send + Clone + 'static,
{
    /// Builds a repository over a live [`Client`], a [`TableConfig`], a
    /// [`RowMapper`] (or row-mapping closure), and an `inserter` that
    /// renders a `T` to an upsert `(sql, params)` whose `RETURNING` clause
    /// projects exactly [`TableConfig::columns`].
    ///
    /// The [`Client`] is shared behind an [`Arc`]; clone the repository
    /// freely (it is cheap and `Send + Sync`).
    pub fn new(
        client: Arc<Client>,
        config: TableConfig,
        mapper: impl RowMapper<T> + 'static,
        inserter: impl Fn(&T) -> (String, Vec<Box<dyn ToSql + Sync + Send>>) + Send + Sync + 'static,
    ) -> Self {
        PostgresReactiveRepository {
            client,
            config,
            mapper: Arc::new(mapper),
            inserter: Arc::new(inserter),
            _id: std::marker::PhantomData,
        }
    }

    /// Streams the rows of `sql` (bound to `params`) as a lazy [`Flux<T>`],
    /// decoding each row through the [`RowMapper`] as it arrives.
    ///
    /// This is the streaming primitive every read is built on, and the
    /// hook for custom derived queries: pass any `SELECT` projecting
    /// [`TableConfig::columns`]. Rows are **not** collected first — the
    /// returned `Flux` is driven directly off the driver's
    /// [`RowStream`](tokio_postgres::RowStream).
    pub fn stream_query(&self, sql: String, params: Vec<Box<dyn ToSql + Sync + Send>>) -> Flux<T> {
        let client = Arc::clone(&self.client);
        let mapper = Arc::clone(&self.mapper);
        Flux::from_stream(async_stream::try_stream! {
            // Bind params as &dyn ToSql for query_raw's ExactSizeIterator.
            let slots: Vec<&(dyn ToSql + Sync)> =
                params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
            let row_stream = client
                .query_raw(sql.as_str(), slots)
                .await
                .map_err(map_pg_err)?;
            futures::pin_mut!(row_stream);
            // Each row is yielded the moment it arrives — no buffering.
            while let Some(row) = futures::StreamExt::next(&mut row_stream).await {
                let row = row.map_err(map_pg_err)?;
                yield mapper.map_row(&row)?;
            }
        })
    }

    fn select_by_id_sql(&self) -> String {
        format!(
            "{} WHERE \"{}\" = $1",
            self.config.select_all_sql(),
            self.config.id_column
        )
    }
}

#[async_trait]
impl<T, ID> ReactiveCrudRepository<T, ID> for PostgresReactiveRepository<T, ID>
where
    T: Send + Sync + 'static,
    ID: ToSql + Sync + Send + Clone + 'static,
{
    fn find_all(&self) -> Flux<T> {
        self.stream_query(self.config.select_all_sql(), Vec::new())
    }

    fn find_all_by_id(&self, ids: Vec<ID>) -> Flux<T> {
        if ids.is_empty() {
            return Flux::empty();
        }
        // WHERE "id" = ANY($1) keeps it a single bound parameter.
        let sql = format!(
            "{} WHERE \"{}\" = ANY($1)",
            self.config.select_all_sql(),
            self.config.id_column
        );
        let params: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(ids)];
        self.stream_query(sql, params)
    }

    fn find_by_id(&self, id: ID) -> Mono<T> {
        let client = Arc::clone(&self.client);
        let mapper = Arc::clone(&self.mapper);
        let sql = self.select_by_id_sql();
        Mono::from_result_future(async move {
            let row = client
                .query_opt(sql.as_str(), &[&id])
                .await
                .map_err(map_pg_err)?;
            match row {
                Some(r) => mapper.map_row(&r),
                None => Err(empty_sentinel()),
            }
        })
        // Translate the "no row" sentinel into an empty Mono (Mono.empty()).
        .on_error_resume(|e| {
            if e.code == EMPTY_CODE {
                Mono::empty()
            } else {
                Mono::error(e)
            }
        })
    }

    fn exists_by_id(&self, id: ID) -> Mono<bool> {
        let client = Arc::clone(&self.client);
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM \"{}\" WHERE \"{}\" = $1)",
            self.config.table, self.config.id_column
        );
        Mono::from_result_future(async move {
            let row = client
                .query_one(sql.as_str(), &[&id])
                .await
                .map_err(map_pg_err)?;
            let exists: bool = row.try_get(0).map_err(map_pg_err)?;
            Ok(exists)
        })
    }

    fn save(&self, entity: T) -> Mono<T> {
        let client = Arc::clone(&self.client);
        let mapper = Arc::clone(&self.mapper);
        let (sql, params) = (self.inserter)(&entity);
        Mono::from_result_future(async move {
            let slots: Vec<&(dyn ToSql + Sync)> = params
                .iter()
                .map(|p| p.as_ref() as &(dyn ToSql + Sync))
                .collect();
            let row = client
                .query_one(sql.as_str(), &slots)
                .await
                .map_err(map_pg_err)?;
            mapper.map_row(&row)
        })
    }

    fn save_all(&self, entities: Vec<T>) -> Flux<T> {
        let client = Arc::clone(&self.client);
        let mapper = Arc::clone(&self.mapper);
        let inserter = Arc::clone(&self.inserter);
        // Each row is upserted and re-emitted in order as it lands.
        Flux::from_stream(async_stream::try_stream! {
            for entity in entities {
                let (sql, params) = inserter(&entity);
                let slots: Vec<&(dyn ToSql + Sync)> =
                    params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
                let row = client
                    .query_one(sql.as_str(), &slots)
                    .await
                    .map_err(map_pg_err)?;
                yield mapper.map_row(&row)?;
            }
        })
    }

    fn delete_by_id(&self, id: ID) -> Mono<()> {
        let client = Arc::clone(&self.client);
        let sql = format!(
            "DELETE FROM \"{}\" WHERE \"{}\" = $1",
            self.config.table, self.config.id_column
        );
        Mono::from_result_future(async move {
            client
                .execute(sql.as_str(), &[&id])
                .await
                .map_err(map_pg_err)?;
            Ok(())
        })
    }

    fn delete_all(&self) -> Mono<()> {
        let client = Arc::clone(&self.client);
        let sql = format!("DELETE FROM \"{}\"", self.config.table);
        Mono::from_result_future(async move {
            client
                .execute(sql.as_str(), &[])
                .await
                .map_err(map_pg_err)?;
            Ok(())
        })
    }

    fn count(&self) -> Mono<u64> {
        let client = Arc::clone(&self.client);
        let sql = format!("SELECT COUNT(*) FROM \"{}\"", self.config.table);
        Mono::from_result_future(async move {
            let row = client
                .query_one(sql.as_str(), &[])
                .await
                .map_err(map_pg_err)?;
            let n: i64 = row.try_get(0).map_err(map_pg_err)?;
            Ok(n as u64)
        })
    }
}

/// Maps a `tokio-postgres` error into a 500 [`FireflyError`].
fn map_pg_err(e: tokio_postgres::Error) -> FireflyError {
    FireflyError::internal(format!("firefly/data: postgres: {e}"))
}

/// A private code used to signal "no row" out of a fallible future, which
/// is then folded into an empty `Mono` by `find_by_id`. It is never
/// surfaced to callers ([`FireflyError`] is not `Clone`, so the sentinel is
/// rebuilt each time rather than shared).
const EMPTY_CODE: &str = "FIREFLY_DATA_EMPTY";

fn empty_sentinel() -> FireflyError {
    FireflyError::new(EMPTY_CODE, "Empty", 404, "no row")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct User {
        id: String,
        name: String,
    }

    fn new_repo() -> ReactiveMemoryRepository<User, String> {
        ReactiveMemoryRepository::new(|u: &User| u.id.clone())
    }

    fn user(id: &str, name: &str) -> User {
        User {
            id: id.into(),
            name: name.into(),
        }
    }

    /// Full CRUD round-trip over the in-memory reactive repository,
    /// driven via `block` / `collect_list` (the Reactor `block()` analog).
    #[tokio::test]
    async fn reactive_memory_full_crud() {
        let r = new_repo();

        // Empty store: find_by_id is an empty Mono, count is 0.
        assert_eq!(r.find_by_id("x".into()).block().await.unwrap(), None);
        assert!(!r.exists_by_id("x".into()).block().await.unwrap().unwrap());
        assert_eq!(r.count().block().await.unwrap(), Some(0));

        // save -> emits the persisted value.
        let saved = r.save(user("u1", "alice")).block().await.unwrap();
        assert_eq!(saved, Some(user("u1", "alice")));

        // find_by_id hit.
        assert_eq!(
            r.find_by_id("u1".into()).block().await.unwrap(),
            Some(user("u1", "alice"))
        );
        assert!(r.exists_by_id("u1".into()).block().await.unwrap().unwrap());

        // save_all -> streams every persisted entity.
        let mut all_saved = r
            .save_all(vec![user("u2", "bob"), user("u3", "carol")])
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        all_saved.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(all_saved, vec![user("u2", "bob"), user("u3", "carol")]);

        // count + find_all.
        assert_eq!(r.count().block().await.unwrap(), Some(3));
        let mut all = r.find_all().collect_list().block().await.unwrap().unwrap();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(all.len(), 3);

        // find_all_by_id selects a subset, skipping the missing id.
        let mut subset = r
            .find_all_by_id(vec!["u1".into(), "u3".into(), "ghost".into()])
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        subset.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(subset, vec![user("u1", "alice"), user("u3", "carol")]);

        // delete_by_id then the row is gone.
        r.delete_by_id("u1".into()).block().await.unwrap();
        assert_eq!(r.find_by_id("u1".into()).block().await.unwrap(), None);
        assert_eq!(r.count().block().await.unwrap(), Some(2));

        // delete_all empties the store.
        r.delete_all().block().await.unwrap();
        assert_eq!(r.count().block().await.unwrap(), Some(0));
        assert!(r
            .find_all()
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap()
            .is_empty());
    }

    /// `save` upserts by id, matching the blocking repository.
    #[tokio::test]
    async fn reactive_memory_save_is_upsert() {
        let r = new_repo();
        r.save(user("u1", "alice")).block().await.unwrap();
        r.save(user("u1", "bob")).block().await.unwrap();
        assert_eq!(
            r.find_by_id("u1".into()).block().await.unwrap(),
            Some(user("u1", "bob"))
        );
        assert_eq!(r.count().block().await.unwrap(), Some(1));
    }

    /// Deleting a missing id is not an error (Spring Data parity).
    #[tokio::test]
    async fn reactive_memory_delete_missing_is_ok() {
        let r = new_repo();
        assert_eq!(
            r.delete_by_id("ghost".into()).block().await.unwrap(),
            Some(())
        );
    }

    /// `find_all_by_id` on an empty id list yields an empty stream.
    #[tokio::test]
    async fn reactive_memory_find_all_by_id_empty() {
        let r = new_repo();
        r.save(user("u1", "alice")).block().await.unwrap();
        let out = r
            .find_all_by_id(Vec::new())
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert!(out.is_empty());
    }

    /// The reactive repository is object-safe behind `dyn`.
    #[tokio::test]
    async fn reactive_memory_is_object_safe() {
        let r: Box<dyn ReactiveCrudRepository<User, String>> = Box::new(new_repo());
        r.save(user("u1", "alice")).block().await.unwrap();
        assert_eq!(
            r.find_by_id("u1".into()).block().await.unwrap(),
            Some(user("u1", "alice"))
        );
    }

    /// The in-memory reactive repository is `Send + Sync` and `Clone`,
    /// so it can be shared across tokio tasks.
    #[test]
    fn reactive_memory_is_send_sync_clone() {
        fn assert_send_sync<X: Send + Sync + Clone>() {}
        assert_send_sync::<ReactiveMemoryRepository<User, String>>();
    }

    /// The reactive specification surface streams matching rows as a
    /// `Flux`, with an optional pageable window.
    #[tokio::test]
    async fn reactive_specification_streams_matches() {
        use crate::{Op, Pageable, Predicate, Specification};
        use serde::Serialize;

        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        struct Acct {
            id: String,
            status: String,
        }

        let r = ReactiveMemoryRepository::new(|a: &Acct| a.id.clone());
        for (i, status) in ["open", "open", "closed", "open"].iter().enumerate() {
            r.save(Acct {
                id: format!("a{i}"),
                status: (*status).into(),
            })
            .block()
            .await
            .unwrap();
        }

        let spec = Specification::pred(Predicate::new("status", Op::Eq, "open"));

        // Unpaged: all three "open" rows.
        let open = r
            .find_by_spec(spec.clone())
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.len(), 3);
        assert!(open.iter().all(|a| a.status == "open"));

        // Paged: page 1 (1-based), size 2 -> at most 2 of the matches.
        let page1 = r
            .find_by_spec_paged(spec, Pageable::paged(1, 2).unwrap())
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page1.len(), 2);
    }

    // ---- Postgres repository: env-gated real round-trip (W4 runs it). ----

    /// Builds the canonical `users` Postgres reactive repository used by
    /// the README example and the ignored integration test below.
    #[allow(dead_code)]
    fn users_pg(client: Arc<Client>) -> PostgresReactiveRepository<User, String> {
        PostgresReactiveRepository::new(
            client,
            TableConfig::new("users", "id", ["id", "name"]),
            // RowMapper: decode (id, name).
            |row: &Row| {
                Ok(User {
                    id: row.try_get("id").map_err(map_pg_err)?,
                    name: row.try_get("name").map_err(map_pg_err)?,
                })
            },
            // inserter: upsert RETURNING the projected columns.
            |u: &User| {
                (
                    "INSERT INTO \"users\" (\"id\", \"name\") VALUES ($1, $2) \
                     ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\" \
                     RETURNING \"id\", \"name\""
                        .to_string(),
                    vec![
                        Box::new(u.id.clone()) as Box<dyn ToSql + Sync + Send>,
                        Box::new(u.name.clone()) as Box<dyn ToSql + Sync + Send>,
                    ],
                )
            },
        )
    }

    /// Real Postgres round-trip. Streams rows lazily as a `Flux`. Env-gated:
    /// reads `FIREFLY_TEST_POSTGRES_URL` (fallbacks `DATABASE_URL` /
    /// `POSTGRES_URL`) and runs against live infra; skips cleanly when unset so
    /// `cargo test` stays green on a bare machine.
    #[tokio::test]
    async fn postgres_reactive_round_trip() {
        let Ok(url) = std::env::var("FIREFLY_TEST_POSTGRES_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .or_else(|_| std::env::var("POSTGRES_URL"))
        else {
            eprintln!(
                "skipping postgres_reactive_round_trip: set FIREFLY_TEST_POSTGRES_URL to run"
            );
            return;
        };

        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .expect("connect");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let client = Arc::new(client);

        // Fresh table per run.
        client
            .batch_execute(
                "DROP TABLE IF EXISTS \"users\"; \
                 CREATE TABLE \"users\" (\"id\" TEXT PRIMARY KEY, \"name\" TEXT NOT NULL);",
            )
            .await
            .expect("create table");

        let repo = users_pg(Arc::clone(&client));

        // save + find_by_id.
        let saved = repo.save(user("u1", "alice")).block().await.unwrap();
        assert_eq!(saved, Some(user("u1", "alice")));
        assert_eq!(
            repo.find_by_id("u1".into()).block().await.unwrap(),
            Some(user("u1", "alice"))
        );
        assert!(repo
            .exists_by_id("u1".into())
            .block()
            .await
            .unwrap()
            .unwrap());
        assert_eq!(repo.find_by_id("ghost".into()).block().await.unwrap(), None);

        // save_all + streaming find_all.
        repo.save_all(vec![user("u2", "bob"), user("u3", "carol")])
            .collect_list()
            .block()
            .await
            .unwrap();
        assert_eq!(repo.count().block().await.unwrap(), Some(3));

        let mut all = repo
            .find_all()
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(all.len(), 3);

        let mut subset = repo
            .find_all_by_id(vec!["u1".into(), "u3".into()])
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        subset.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(subset, vec![user("u1", "alice"), user("u3", "carol")]);

        // delete_by_id + delete_all.
        repo.delete_by_id("u1".into()).block().await.unwrap();
        assert_eq!(repo.count().block().await.unwrap(), Some(2));
        repo.delete_all().block().await.unwrap();
        assert_eq!(repo.count().block().await.unwrap(), Some(0));
    }

    /// `ReactiveSortingRepository` (Spring's `findAll(Sort)` / `findAll(Pageable)`)
    /// over the in-memory repository: orders and windows correctly.
    #[tokio::test]
    async fn reactive_sorting_repository_orders_and_pages() {
        #[derive(Debug, Clone, PartialEq, serde::Serialize)]
        struct Item {
            id: i64,
            name: String,
        }
        let repo = ReactiveMemoryRepository::new(|i: &Item| i.id);
        for (id, name) in [(1, "charlie"), (2, "alice"), (3, "bob")] {
            repo.save(Item {
                id,
                name: name.into(),
            })
            .block()
            .await
            .unwrap();
        }

        // find_all_sorted(name asc)
        let asc = repo
            .find_all_sorted(crate::RequestSort::by(["name"]))
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            asc.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["alice", "bob", "charlie"]
        );

        // find_all_sorted(name desc)
        let desc = repo
            .find_all_sorted(crate::RequestSort::by(["name"]).descending())
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(desc.first().unwrap().name, "charlie");

        // find_all_paged(page 1, size 2, sorted by name) → first window
        let page = repo
            .find_all_paged(crate::Pageable::of(1, 2, crate::RequestSort::by(["name"])).unwrap())
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].name, "alice");
        assert_eq!(page[1].name, "bob");
    }
}
