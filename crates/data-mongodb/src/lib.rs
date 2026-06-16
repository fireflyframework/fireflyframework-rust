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

//! firefly-data-mongodb — the **document** repository adapter that
//! implements the firefly-data ports over the official `mongodb` crate.
//!
//! This crate is the Rust port of pyfly's
//! `pyfly.data.document.mongodb` package. It puts a MongoDB document
//! store behind the **same** reactive repository surface as the
//! relational adapters — [`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository)
//! and [`ReactiveSpecificationRepository`](firefly_data::ReactiveSpecificationRepository)
//! — so a service can swap a Postgres repository for a Mongo one without
//! touching its call sites. That is the whole point of the hexagonal
//! split: the [`Specification`](firefly_data::Specification) tree is the
//! single source of truth, and
//! [`Specification::to_mongo`](firefly_data::Specification::to_mongo)
//! lowers it to a MongoDB `$`-operator filter exactly as `to_sql` lowers
//! it for relational stores.
//!
//! # What it provides
//!
//! - [`MongoRepository<T, ID>`] — a generic CRUD + specification +
//!   paging repository over a `mongodb` collection, where
//!   `T: Serialize + DeserializeOwned`. It implements
//!   [`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository)
//!   (`find_all` / `find_by_id` / `exists_by_id` / `save` / `save_all` /
//!   `delete_by_id` / `delete_all` / `count`) and
//!   [`ReactiveSpecificationRepository`](firefly_data::ReactiveSpecificationRepository)
//!   (`find_by_spec` / `find_by_spec_paged`), plus
//!   [`MongoRepository::find_page`] returning the canonical
//!   [`Page`](firefly_data::Page) envelope. Reads stream lazily off the
//!   driver cursor.
//! - [`BaseDocument`] — the audit-stamp + soft-delete mixin every
//!   document embeds (`#[serde(flatten)]`), the Rust analogue of pyfly's
//!   `BaseDocument`. Stamping is delegated to firefly-data's
//!   [`Auditor`](firefly_data::Auditor), so audit semantics match the
//!   relational adapter exactly.
//! - [`Audited`] — the hook by which a document exposes its
//!   [`BaseDocument`] so [`MongoRepository::save_audited`] /
//!   [`MongoRepository::save_all_audited`] can auto-stamp on write.
//! - **Automatic soft-delete filtering**: wire a
//!   [`SoftDeletePolicy`](firefly_data::SoftDeletePolicy) with
//!   [`MongoRepository::with_soft_delete`] and every read injects a
//!   `{"<column>": null}` guard while `delete_by_id` becomes a logical
//!   delete.
//! - **Derived & custom queries executed end-to-end**: the document analogue
//!   of pyfly's repository bean post-processor —
//!   [`find_by_derived`](MongoRepository::find_by_derived) /
//!   [`count_by_derived`](MongoRepository::count_by_derived) /
//!   [`exists_by_derived`](MongoRepository::exists_by_derived) /
//!   [`delete_by_derived`](MongoRepository::delete_by_derived) run a parsed
//!   `find_by_…`-style method name (lowered to a `$`-operator filter via the
//!   shared [`Specification`](firefly_data::Specification) tree), while
//!   [`query_find`](MongoRepository::query_find) /
//!   [`query_aggregate`](MongoRepository::query_aggregate) run a `@query`
//!   JSON filter document / aggregation pipeline with `":param"` named
//!   substitution, and [`project_by_spec`](MongoRepository::project_by_spec)
//!   applies a DB-level [`ColumnProjection`](firefly_data::ColumnProjection).
//! - **Actuator integration** (feature `actuator`): [`MongoHealthIndicator`]
//!   contributes a `db` component to `GET /actuator/health` (server `ping`).
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_data::{
//!     Op, Predicate, ReactiveCrudRepository, ReactiveSpecificationRepository, Specification,
//! };
//! use firefly_data_mongodb::{BaseDocument, MongoRepository};
//! use mongodb::bson::{Bson, Document};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct UserDocument {
//!     #[serde(rename = "_id")]
//!     id: String,
//!     name: String,
//!     #[serde(flatten)]
//!     base: BaseDocument,
//! }
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
//! let collection = client.database("app").collection::<Document>("users");
//!
//! // The id_extractor reads the entity's `_id` as a BSON value.
//! let repo: MongoRepository<UserDocument, String> =
//!     MongoRepository::new(collection, |u: &UserDocument| Bson::String(u.id.clone()));
//!
//! let user = UserDocument {
//!     id: "u1".into(),
//!     name: "alice".into(),
//!     base: BaseDocument::new(),
//! };
//! repo.save(user).block().await?;
//!
//! // The SAME Specification tree that drives SQL drives Mongo here.
//! let spec = Specification::pred(Predicate::new("name", Op::Eq, "alice"));
//! let hits = repo.find_by_spec(spec).collect_list().block().await?;
//! # let _ = hits;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod document;
mod error;
#[cfg(feature = "actuator")]
mod observe;
mod repository;

pub use document::BaseDocument;
pub use repository::{Audited, MongoRepository};

#[cfg(feature = "actuator")]
pub use observe::MongoHealthIndicator;

/// Framework version stamp.
pub const VERSION: &str = "26.6.23";
