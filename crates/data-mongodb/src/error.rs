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

//! Error mapping for the MongoDB adapter.
//!
//! Every failure surfaced through the reactive
//! [`Mono`](firefly_reactive::Mono) / [`Flux`](firefly_reactive::Flux)
//! channel is a [`FireflyError`](firefly_kernel::FireflyError), so the
//! MongoDB adapter folds the three failure sources it can hit — the
//! `mongodb` driver, BSON (de)serialisation, and our own invariant
//! checks — into that single type. The framework's RFC 7807 layer then
//! renders them as `500`s.

use firefly_kernel::FireflyError;

/// Maps a `mongodb` driver error into an internal [`FireflyError`].
///
/// Used everywhere a driver call (`find`, `insert_one`, `replace_one`,
/// `count_documents`, cursor advance, …) can fail.
pub(crate) fn map_mongo_err(e: mongodb::error::Error) -> FireflyError {
    FireflyError::internal(format!("firefly/data-mongodb: mongodb: {e}"))
}

/// Maps a BSON serialisation error (`bson::to_document` /
/// `bson::to_bson`) into an internal [`FireflyError`].
pub(crate) fn map_ser_err(e: mongodb::bson::ser::Error) -> FireflyError {
    FireflyError::internal(format!("firefly/data-mongodb: bson serialise: {e}"))
}

/// Maps a BSON deserialisation error (`bson::from_document`) into an
/// internal [`FireflyError`].
pub(crate) fn map_de_err(e: mongodb::bson::de::Error) -> FireflyError {
    FireflyError::internal(format!("firefly/data-mongodb: bson deserialise: {e}"))
}
