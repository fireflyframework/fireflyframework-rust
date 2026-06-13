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

//! The MongoDB **document** repository — the `mongodb`-crate adapter that
//! implements the firefly-data ports behind the *same*
//! [`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository) /
//! [`ReactiveSpecificationRepository`](firefly_data::ReactiveSpecificationRepository)
//! surface as the relational adapters.
//!
//! This is the Rust port of pyfly's `MongoRepository` (and `Repository`
//! surface). Where pyfly drives Beanie ODM, the Rust adapter drives the
//! official `mongodb` crate directly, and where pyfly returns plain
//! awaited values the Rust adapter returns
//! [`Mono`](firefly_reactive::Mono) / [`Flux`](firefly_reactive::Flux) so
//! the document store plugs into the same reactive composition as Postgres
//! and sqlx. Reads stream lazily off the driver's cursor; nothing is
//! buffered before the first row.

use std::marker::PhantomData;

use async_trait::async_trait;
use firefly_data::{
    Auditor, Pageable, ReactiveCrudRepository, ReactiveSpecificationRepository, SoftDeletePolicy,
    Specification,
};
use firefly_reactive::{Flux, Mono};
use mongodb::bson::{doc, to_bson, to_document, Bson, Document};
use mongodb::options::{FindOptions, ReplaceOptions};
use mongodb::Collection;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{map_de_err, map_mongo_err, map_ser_err};

/// The MongoDB `_id` field name — the document primary-key key.
const ID_FIELD: &str = "_id";

/// A generic **document** repository over the official `mongodb` crate —
/// the document-store analogue of the relational
/// [`PostgresReactiveRepository`](firefly_data::PostgresReactiveRepository)
/// and the Rust port of pyfly's `MongoRepository[T, ID]`.
///
/// `MongoRepository<T, ID>` implements the exact same firefly-data
/// reactive ports as every relational adapter
/// ([`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository) and
/// [`ReactiveSpecificationRepository`](firefly_data::ReactiveSpecificationRepository)),
/// so a service can swap a Postgres repository for a Mongo one without
/// touching call sites. The document type `T` must be
/// `Serialize + DeserializeOwned` (the BSON (de)serialisation bound), and
/// the id type `ID` must serialise to a BSON value so it can be matched
/// against `_id`.
///
/// On top of the base CRUD surface it adds, mirroring pyfly:
///
/// - [`find_by_spec`](Self::find_by_spec) /
///   [`find_by_spec_paged`](Self::find_by_spec_paged), consuming a
///   firefly-data [`Specification`] lowered to a Mongo `$`-operator filter
///   via [`Specification::to_mongo`];
/// - [`find_page`](Self::find_page), returning the canonical
///   [`Page`](firefly_data::Page) envelope with `sort` / `skip` / `limit`
///   derived from a [`Pageable`];
/// - **automatic audit stamping** on every write when an
///   [`Auditor`](firefly_data::Auditor) is wired
///   ([`with_auditor`](Self::with_auditor)); the entity carries the audit
///   fields via [`BaseDocument`](crate::BaseDocument) and exposes them
///   through the [`Audited`] hook;
/// - **automatic soft-delete filtering** when a
///   [`SoftDeletePolicy`](firefly_data::SoftDeletePolicy) is wired
///   ([`with_soft_delete`](Self::with_soft_delete)): every read injects a
///   `{"<column>": null}` guard so logically deleted documents stay
///   hidden, and [`delete_by_id`](ReactiveCrudRepository::delete_by_id)
///   becomes a soft delete (a `$set` of the stamp) rather than a physical
///   removal.
///
/// The [`Collection`] is `Clone` (cheap — it shares the client), so the
/// repository is cheaply cloneable and `Send + Sync`.
pub struct MongoRepository<T, ID> {
    collection: Collection<Document>,
    auditor: Option<Auditor>,
    soft_delete: Option<SoftDeletePolicy>,
    id_extractor: Box<dyn Fn(&T) -> Bson + Send + Sync>,
    _marker: PhantomData<fn() -> (T, ID)>,
}

impl<T, ID> MongoRepository<T, ID>
where
    T: Serialize + DeserializeOwned + Send + Sync + Unpin + 'static,
    ID: Serialize + Send + Sync + 'static,
{
    /// Builds a repository over a `mongodb` [`Collection`] and an
    /// `id_extractor` that reads the entity's primary key as a BSON value
    /// (the value matched against `_id`, and the value used to address a
    /// document for upsert / soft-delete).
    ///
    /// The collection is typed as `Collection<Document>` internally so the
    /// repository can inject query guards and stamp audit fields uniformly
    /// regardless of `T`'s concrete shape; documents are (de)serialised to
    /// `T` at the boundary.
    pub fn new(
        collection: Collection<Document>,
        id_extractor: impl Fn(&T) -> Bson + Send + Sync + 'static,
    ) -> Self {
        MongoRepository {
            collection,
            auditor: None,
            soft_delete: None,
            id_extractor: Box::new(id_extractor),
            _marker: PhantomData,
        }
    }

    /// Wires an [`Auditor`](firefly_data::Auditor) so every
    /// [`save`](ReactiveCrudRepository::save) /
    /// [`save_all`](ReactiveCrudRepository::save_all) auto-stamps the
    /// entity's audit fields (the entity must carry a
    /// [`BaseDocument`](crate::BaseDocument) and expose it via [`Audited`]).
    /// The Rust analogue of pyfly's `AuditingEntityListener`.
    pub fn with_auditor(mut self, auditor: Auditor) -> Self {
        self.auditor = Some(auditor);
        self
    }

    /// Wires a [`SoftDeletePolicy`](firefly_data::SoftDeletePolicy) so
    /// reads inject a `{"<column>": null}` guard and
    /// [`delete_by_id`](ReactiveCrudRepository::delete_by_id) becomes a
    /// soft delete (a timestamp `$set`) — the behaviour pyfly threads
    /// through its `SoftDeleteRepository` overrides.
    pub fn with_soft_delete(mut self, policy: SoftDeletePolicy) -> Self {
        self.soft_delete = Some(policy);
        self
    }

    /// The wrapped collection (mainly for tests / index management).
    pub fn collection(&self) -> &Collection<Document> {
        &self.collection
    }

    /// AND-combines `filter` with the soft-delete guard (when wired). An
    /// empty filter `{}` plus a guard becomes just the guard; an empty
    /// filter with no guard stays `{}` (match everything).
    fn guarded(&self, filter: Document) -> Document {
        guarded(self.soft_delete.as_ref(), filter)
    }

    /// Converts a [`Specification`]'s lowered JSON filter into a BSON
    /// [`Document`]. The lowering ([`Specification::to_mongo`]) emits a
    /// natural `$`-operator JSON object, which serialises 1:1 into BSON.
    fn spec_to_filter(spec: &Specification) -> Result<Document, firefly_kernel::FireflyError> {
        let json = spec.to_mongo();
        match to_bson(&json).map_err(map_ser_err)? {
            Bson::Document(d) => Ok(d),
            // `to_mongo` always yields an object; any other shape is a no-op.
            _ => Ok(Document::new()),
        }
    }

    /// Builds the `_id` equality filter for `id`, guarded by soft-delete
    /// when wired.
    fn id_filter(&self, id: &ID) -> Result<Document, firefly_kernel::FireflyError> {
        let id_bson = to_bson(id).map_err(map_ser_err)?;
        Ok(self.guarded(doc! { ID_FIELD: id_bson }))
    }

    /// Builds [`FindOptions`] carrying the sort / skip / limit derived
    /// from a [`Pageable`]. An unpaged request omits skip/limit; sort is
    /// always projected (ascending = `1`, descending = `-1`).
    fn find_options(pageable: &Pageable) -> FindOptions {
        let mut sort = Document::new();
        for order in &pageable.sort.orders {
            let dir = match order.direction {
                firefly_data::Direction::Asc => 1_i32,
                firefly_data::Direction::Desc => -1_i32,
            };
            sort.insert(order.property.clone(), dir);
        }
        let mut options = FindOptions::default();
        if !sort.is_empty() {
            options.sort = Some(sort);
        }
        if pageable.is_paged() {
            options.skip = Some(pageable.offset() as u64);
            options.limit = Some(pageable.size as i64);
        }
        options
    }

    /// Deserialises a BSON [`Document`] into `T`.
    fn decode(doc: Document) -> Result<T, firefly_kernel::FireflyError> {
        mongodb::bson::from_document(doc).map_err(map_de_err)
    }

    /// Serialises `entity` into a BSON [`Document`] for storage.
    fn encode(entity: &T) -> Result<Document, firefly_kernel::FireflyError> {
        to_document(entity).map_err(map_ser_err)
    }

    /// Streams every document matching `filter` (guarded), applying
    /// `options`, decoding each as it arrives off the cursor. The
    /// streaming primitive every read is built on.
    fn stream_find(&self, filter: Document, options: Option<FindOptions>) -> Flux<T> {
        let collection = self.collection.clone();
        Flux::from_stream(async_stream::try_stream! {
            let mut find = collection.find(filter);
            if let Some(opts) = options {
                find = find.with_options(opts);
            }
            let mut cursor = find.await.map_err(map_mongo_err)?;
            while let Some(next) = futures::StreamExt::next(&mut cursor).await {
                let raw = next.map_err(map_mongo_err)?;
                yield Self::decode(raw)?;
            }
        })
    }

    /// Counts the documents matching `filter` (guarded).
    fn count_filter(&self, filter: Document) -> Mono<u64> {
        let collection = self.collection.clone();
        Mono::from_result_future(async move {
            collection
                .count_documents(filter)
                .await
                .map_err(map_mongo_err)
        })
    }

    /// Streams every document matching the lowered `spec`, guarded by
    /// soft-delete, with the given `options` — the shared engine behind
    /// [`find_by_spec`](Self::find_by_spec) and
    /// [`find_by_spec_paged`](Self::find_by_spec_paged).
    fn find_spec(&self, spec: Specification, options: Option<FindOptions>) -> Flux<T> {
        let filter = match Self::spec_to_filter(&spec) {
            Ok(f) => self.guarded(f),
            Err(e) => return Flux::error(e),
        };
        self.stream_find(filter, options)
    }

    /// Returns a [`Page`](firefly_data::Page) of documents matching
    /// `spec` for `pageable` — the canonical paged envelope (content +
    /// total + page metadata), mirroring pyfly's
    /// `find_all_by_spec_paged`. The total counts only live (non
    /// soft-deleted) documents matching the spec.
    pub fn find_page(
        &self,
        spec: Specification,
        pageable: Pageable,
    ) -> Mono<firefly_data::Page<T>> {
        let filter = match Self::spec_to_filter(&spec) {
            Ok(f) => self.guarded(f),
            Err(e) => return Mono::error(e),
        };
        let collection = self.collection.clone();
        let options = Self::find_options(&pageable);
        Mono::from_result_future(async move {
            let total = collection
                .count_documents(filter.clone())
                .await
                .map_err(map_mongo_err)?;
            let mut cursor = collection
                .find(filter)
                .with_options(options)
                .await
                .map_err(map_mongo_err)?;
            let mut content = Vec::new();
            while let Some(next) = futures::StreamExt::next(&mut cursor).await {
                let raw = next.map_err(map_mongo_err)?;
                content.push(Self::decode(raw)?);
            }
            // 1-based Pageable page -> 0-based Page number.
            let number = pageable.page.saturating_sub(1);
            Ok(firefly_data::Page::new(
                content,
                number,
                pageable.size,
                total,
            ))
        })
    }

    /// Upserts `entity` by its `_id` (replace-with-upsert) and emits it
    /// back. Audit stamping is the caller's responsibility via
    /// [`BaseDocument`](crate::BaseDocument) before calling, or wire an
    /// [`Auditor`](firefly_data::Auditor) and use [`save_audited`].
    fn upsert(&self, entity: T) -> Mono<T> {
        let collection = self.collection.clone();
        let id = (self.id_extractor)(&entity);
        Mono::from_result_future(async move {
            let document = Self::encode(&entity)?;
            collection
                .replace_one(doc! { ID_FIELD: id }, document)
                .with_options(ReplaceOptions::builder().upsert(true).build())
                .await
                .map_err(map_mongo_err)?;
            Ok(entity)
        })
    }
}

/// The soft-delete guard clause `{"<column>": null}` for a policy, or
/// `None` when soft-delete is off. Free function so it is testable
/// without a live [`Collection`].
fn soft_delete_guard(policy: Option<&SoftDeletePolicy>) -> Option<Document> {
    policy.map(|p| doc! { p.column(): Bson::Null })
}

/// AND-combines `filter` with the soft-delete guard (when a policy is
/// supplied). An empty filter plus a guard becomes just the guard; an
/// empty filter with no guard stays `{}` (match everything). Free
/// function so it is testable without a live [`Collection`].
fn guarded(policy: Option<&SoftDeletePolicy>, filter: Document) -> Document {
    match soft_delete_guard(policy) {
        None => filter,
        Some(guard) if filter.is_empty() => guard,
        Some(guard) => doc! { "$and": [filter, guard] },
    }
}

/// The hook by which a document exposes its embedded
/// [`BaseDocument`](crate::BaseDocument) so the repository can auto-stamp
/// audit fields on write — the Rust analogue of pyfly's
/// `AuditingEntityListener` reaching into the mapped entity.
///
/// Implement it for any entity that embeds a `BaseDocument`; the repository
/// then offers [`MongoRepository::save_audited`] /
/// [`MongoRepository::save_all_audited`], which stamp before persisting.
pub trait Audited {
    /// Mutable access to the entity's embedded
    /// [`BaseDocument`](crate::BaseDocument).
    fn base_mut(&mut self) -> &mut crate::BaseDocument;
}

impl<T, ID> MongoRepository<T, ID>
where
    T: Serialize + DeserializeOwned + Send + Sync + Unpin + Audited + 'static,
    ID: Serialize + Send + Sync + 'static,
{
    /// Upserts `entity`, **auto-stamping** its audit fields first when an
    /// [`Auditor`](firefly_data::Auditor) is wired (insert semantics:
    /// `created_at` / `updated_at` move together). Falls back to a plain
    /// upsert when no auditor is configured. The Rust analogue of pyfly's
    /// `before_insert` listener firing inside `save`.
    pub fn save_audited(&self, mut entity: T) -> Mono<T> {
        if let Some(auditor) = &self.auditor {
            entity.base_mut().stamp_insert(auditor);
        }
        self.upsert(entity)
    }

    /// Upserts every entity in `entities`, auto-stamping each on insert
    /// when an [`Auditor`](firefly_data::Auditor) is wired, and streams
    /// the persisted values back.
    pub fn save_all_audited(&self, entities: Vec<T>) -> Flux<T> {
        let stamped: Vec<T> = entities
            .into_iter()
            .map(|mut e| {
                if let Some(auditor) = &self.auditor {
                    e.base_mut().stamp_insert(auditor);
                }
                e
            })
            .collect();
        // Re-emit each persisted entity in order.
        let saves: Vec<Mono<T>> = stamped.into_iter().map(|e| self.upsert(e)).collect();
        Flux::from_iter(saves).flat_map(1, |m| m.as_flux())
    }
}

#[async_trait]
impl<T, ID> ReactiveCrudRepository<T, ID> for MongoRepository<T, ID>
where
    T: Serialize + DeserializeOwned + Send + Sync + Unpin + 'static,
    ID: Serialize + Send + Sync + Clone + 'static,
{
    fn find_all(&self) -> Flux<T> {
        let filter = self.guarded(Document::new());
        self.stream_find(filter, None)
    }

    fn find_all_by_id(&self, ids: Vec<ID>) -> Flux<T> {
        if ids.is_empty() {
            return Flux::empty();
        }
        let id_bsons: Result<Vec<Bson>, _> = ids
            .iter()
            .map(|id| to_bson(id).map_err(map_ser_err))
            .collect();
        let id_bsons = match id_bsons {
            Ok(v) => v,
            Err(e) => return Flux::error(e),
        };
        let filter = self.guarded(doc! { ID_FIELD: { "$in": id_bsons } });
        self.stream_find(filter, None)
    }

    fn find_by_id(&self, id: ID) -> Mono<T> {
        let collection = self.collection.clone();
        let filter = match self.id_filter(&id) {
            Ok(f) => f,
            Err(e) => return Mono::error(e),
        };
        // `from_raw` yields `Result<Option<T>>`, so a miss maps straight to
        // an empty Mono (Mono.empty()), exactly as Spring Data signals a
        // missing `findById`.
        Mono::from_raw(async move {
            let found = collection.find_one(filter).await.map_err(map_mongo_err)?;
            match found {
                Some(raw) => Ok(Some(Self::decode(raw)?)),
                None => Ok(None),
            }
        })
    }

    fn exists_by_id(&self, id: ID) -> Mono<bool> {
        let collection = self.collection.clone();
        let filter = match self.id_filter(&id) {
            Ok(f) => f,
            Err(e) => return Mono::error(e),
        };
        Mono::from_result_future(async move {
            let n = collection
                .count_documents(filter)
                .await
                .map_err(map_mongo_err)?;
            Ok(n > 0)
        })
    }

    fn save(&self, entity: T) -> Mono<T> {
        self.upsert(entity)
    }

    fn save_all(&self, entities: Vec<T>) -> Flux<T> {
        let saves: Vec<Mono<T>> = entities.into_iter().map(|e| self.upsert(e)).collect();
        Flux::from_iter(saves).flat_map(1, |m| m.as_flux())
    }

    fn delete_by_id(&self, id: ID) -> Mono<()> {
        let collection = self.collection.clone();
        let id_bson = match to_bson(&id).map_err(map_ser_err) {
            Ok(b) => b,
            Err(e) => return Mono::error(e),
        };
        // Soft delete when a policy is wired: $set the stamp column to now.
        if let Some(policy) = &self.soft_delete {
            let column = policy.column().to_string();
            let now: Bson = match to_bson(&chrono::Utc::now()).map_err(map_ser_err) {
                Ok(b) => b,
                Err(e) => return Mono::error(e),
            };
            return Mono::from_result_future(async move {
                collection
                    .update_one(doc! { ID_FIELD: id_bson }, doc! { "$set": { column: now } })
                    .await
                    .map_err(map_mongo_err)?;
                Ok(())
            });
        }
        // Hard delete otherwise.
        Mono::from_result_future(async move {
            collection
                .delete_one(doc! { ID_FIELD: id_bson })
                .await
                .map_err(map_mongo_err)?;
            Ok(())
        })
    }

    fn delete_all(&self) -> Mono<()> {
        let collection = self.collection.clone();
        // delete_all removes everything physically (matches Spring Data's
        // deleteAll), regardless of the soft-delete guard.
        Mono::from_result_future(async move {
            collection
                .delete_many(Document::new())
                .await
                .map_err(map_mongo_err)?;
            Ok(())
        })
    }

    fn count(&self) -> Mono<u64> {
        self.count_filter(self.guarded(Document::new()))
    }
}

#[async_trait]
impl<T, ID> ReactiveSpecificationRepository<T> for MongoRepository<T, ID>
where
    T: Serialize + DeserializeOwned + Send + Sync + Unpin + 'static,
    ID: Serialize + Send + Sync + Clone + 'static,
{
    fn find_by_spec(&self, spec: Specification) -> Flux<T> {
        self.find_spec(spec, None)
    }

    fn find_by_spec_paged(&self, spec: Specification, pageable: Pageable) -> Flux<T> {
        let options = Self::find_options(&pageable);
        self.find_spec(spec, Some(options))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_data::{Op, Order, Predicate, RequestSort};

    // ---- Pure unit tests (no MongoDB needed): filter / options shapes ----

    /// The soft-delete guard renders the column = null clause.
    #[test]
    fn soft_delete_guard_shape() {
        let policy = SoftDeletePolicy::new();
        let guard = soft_delete_guard(Some(&policy)).unwrap();
        assert_eq!(guard, doc! { "deleted_at": Bson::Null });
    }

    /// Guarding an empty filter yields just the guard; guarding a real
    /// filter ANDs the two.
    #[test]
    fn guarded_combines_with_and() {
        let policy = SoftDeletePolicy::new();
        assert_eq!(
            guarded(Some(&policy), Document::new()),
            doc! { "deleted_at": Bson::Null }
        );
        let user = doc! { "name": "alice" };
        assert_eq!(
            guarded(Some(&policy), user.clone()),
            doc! { "$and": [ user, { "deleted_at": Bson::Null } ] }
        );
    }

    /// A custom soft-delete column is honoured in the guard.
    #[test]
    fn guarded_honours_custom_column() {
        let policy = SoftDeletePolicy::for_column("removedAt");
        assert_eq!(
            guarded(Some(&policy), Document::new()),
            doc! { "removedAt": Bson::Null }
        );
    }

    /// With no soft-delete policy, the filter is returned untouched.
    #[test]
    fn guarded_is_identity_without_policy() {
        assert!(soft_delete_guard(None).is_none());
        assert_eq!(guarded(None, Document::new()), Document::new());
    }

    /// A Specification lowers to the matching BSON filter document.
    #[test]
    fn spec_lowers_to_bson_filter() {
        type Repo = MongoRepository<serde_json::Value, String>;
        let spec = Specification::pred(Predicate::new("role", Op::Eq, "admin"))
            & Specification::pred(Predicate::new("active", Op::Eq, true));
        let filter = Repo::spec_to_filter(&spec).unwrap();
        assert_eq!(
            filter,
            doc! { "$and": [ { "role": { "$eq": "admin" } }, { "active": { "$eq": true } } ] }
        );
    }

    /// `find_options` projects sort (asc=1/desc=-1) plus skip/limit from a
    /// Pageable.
    #[test]
    fn find_options_projects_sort_and_window() {
        type Repo = MongoRepository<serde_json::Value, String>;
        let sort = RequestSort::of([Order::asc("name"), Order::desc("age")]);
        let pageable = Pageable::of(3, 10, sort).unwrap();
        let opts = Repo::find_options(&pageable);
        assert_eq!(opts.sort, Some(doc! { "name": 1, "age": -1 }));
        assert_eq!(opts.skip, Some(20)); // (3-1)*10
        assert_eq!(opts.limit, Some(10));
    }

    /// An unpaged request omits skip/limit.
    #[test]
    fn find_options_unpaged_has_no_window() {
        type Repo = MongoRepository<serde_json::Value, String>;
        let opts = Repo::find_options(&Pageable::unpaged());
        assert_eq!(opts.skip, None);
        assert_eq!(opts.limit, None);
        assert_eq!(opts.sort, None);
    }

    /// The repository is object-safe behind `dyn` for both ports.
    #[test]
    fn is_object_safe() {
        fn _crud(r: Box<dyn ReactiveCrudRepository<serde_json::Value, String>>) {
            let _ = r;
        }
        fn _spec(r: Box<dyn ReactiveSpecificationRepository<serde_json::Value>>) {
            let _ = r;
        }
    }

    /// `Send + Sync` so it can be shared across tokio tasks.
    #[test]
    fn is_send_sync() {
        fn assert_send_sync<X: Send + Sync>() {}
        assert_send_sync::<MongoRepository<serde_json::Value, String>>();
    }

    // ---- MongoDB repository: env-gated real round-trip (W4 runs it). ----

    use crate::BaseDocument;
    use firefly_data::{Auditor, Order as SortOrder, Page, UserProvider};
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct UserDoc {
        #[serde(rename = "_id")]
        id: String,
        name: String,
        role: String,
        #[serde(flatten)]
        base: BaseDocument,
    }

    impl UserDoc {
        fn new(id: &str, name: &str, role: &str) -> Self {
            UserDoc {
                id: id.into(),
                name: name.into(),
                role: role.into(),
                base: BaseDocument::new(),
            }
        }
    }

    impl Audited for UserDoc {
        fn base_mut(&mut self) -> &mut BaseDocument {
            &mut self.base
        }
    }

    /// Full MongoDB round-trip exercising every port method, spec / paging
    /// queries, audit stamping, and soft-delete filtering. Env-gated:
    /// reads `FIREFLY_TEST_MONGODB_URL` (fallback `MONGODB_URL`) and runs
    /// against live infra; skips cleanly when unset so `cargo test` stays
    /// green on a bare machine.
    #[tokio::test]
    async fn mongodb_round_trip() {
        let Ok(url) =
            std::env::var("FIREFLY_TEST_MONGODB_URL").or_else(|_| std::env::var("MONGODB_URL"))
        else {
            eprintln!("skipping mongodb_round_trip: set FIREFLY_TEST_MONGODB_URL to run");
            return;
        };

        let client = mongodb::Client::with_uri_str(&url).await.expect("connect");
        let collection = client
            .database("firefly_test")
            .collection::<Document>("data_mongodb_users");
        // Fresh collection per run.
        collection.drop().await.expect("drop");

        let provider: UserProvider = Arc::new(|| Some("tester".to_string()));
        let repo: MongoRepository<UserDoc, String> =
            MongoRepository::new(collection.clone(), |u: &UserDoc| Bson::String(u.id.clone()))
                .with_auditor(Auditor::with_user_provider(provider))
                .with_soft_delete(SoftDeletePolicy::new());

        // save_audited stamps audit fields on insert.
        let saved = repo
            .save_audited(UserDoc::new("u1", "alice", "admin"))
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.base.audit.created_by.as_deref(), Some("tester"));
        assert!(saved.base.audit.created_at.is_some());

        // find_by_id hit + miss.
        let got = repo.find_by_id("u1".into()).block().await.unwrap().unwrap();
        assert_eq!(got.name, "alice");
        assert_eq!(repo.find_by_id("ghost".into()).block().await.unwrap(), None);
        assert!(repo
            .exists_by_id("u1".into())
            .block()
            .await
            .unwrap()
            .unwrap());

        // save_all_audited streams every persisted entity.
        repo.save_all_audited(vec![
            UserDoc::new("u2", "bob", "user"),
            UserDoc::new("u3", "carol", "admin"),
        ])
        .collect_list()
        .block()
        .await
        .unwrap();
        assert_eq!(repo.count().block().await.unwrap(), Some(3));

        // find_by_spec uses the SAME Specification tree as the SQL path.
        let admins = repo
            .find_by_spec(Specification::pred(Predicate::new("role", Op::Eq, "admin")))
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admins.len(), 2);
        assert!(admins.iter().all(|u| u.role == "admin"));

        // find_by_spec_paged: sorted by name asc, page 1 size 1 -> just "alice".
        let sort = RequestSort::of([SortOrder::asc("name")]);
        let page1 = repo
            .find_by_spec_paged(
                Specification::pred(Predicate::new("role", Op::Eq, "admin")),
                Pageable::of(1, 1, sort.clone()).unwrap(),
            )
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].name, "alice");

        // find_page returns the canonical Page envelope.
        let page: Page<UserDoc> = repo
            .find_page(
                Specification::pred(Predicate::new("role", Op::Eq, "admin")),
                Pageable::of(1, 1, sort).unwrap(),
            )
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page.total_elements, 2);
        assert_eq!(page.total_pages, 2);
        assert_eq!(page.number, 0);
        assert_eq!(page.content.len(), 1);

        // Soft delete: delete_by_id sets deleted_at; the row disappears
        // from every guarded read but stays physically present.
        repo.delete_by_id("u1".into()).block().await.unwrap();
        assert_eq!(repo.find_by_id("u1".into()).block().await.unwrap(), None);
        assert!(!repo
            .exists_by_id("u1".into())
            .block()
            .await
            .unwrap()
            .unwrap());
        assert_eq!(repo.count().block().await.unwrap(), Some(2));
        // The document is still in the collection (raw, unguarded count = 3).
        let raw_total = collection.count_documents(Document::new()).await.unwrap();
        assert_eq!(raw_total, 3);

        // find_all only streams live rows.
        let live = repo
            .find_all()
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.len(), 2);
        assert!(live.iter().all(|u| u.id != "u1"));

        // delete_all physically clears everything.
        repo.delete_all().block().await.unwrap();
        assert_eq!(
            collection.count_documents(Document::new()).await.unwrap(),
            0
        );

        // Cleanup.
        collection.drop().await.expect("drop");
    }
}
