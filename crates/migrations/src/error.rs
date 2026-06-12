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

//! Error types for the migration runner.

use crate::database::DatabaseError;

/// Errors returned by [`run`](crate::run), [`inspect`](crate::inspect),
/// and [`Source::list`](crate::Source::list).
///
/// Display strings mirror the Go port:
/// `"firefly/migrations: checksum mismatch: V1 (V001__init.sql)"`,
/// `"create migrations table: …"`, `"V2 (V002__seed.sql): …"`.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// An applied migration's recorded checksum no longer matches the
    /// on-disk file (i.e. someone edited a committed migration — never
    /// do this; migrations are append-only history).
    #[error("firefly/migrations: checksum mismatch: V{version} ({filename})")]
    ChecksumMismatch {
        /// Version of the offending migration.
        version: i64,
        /// Filename of the offending migration.
        filename: String,
    },

    /// Creating the `firefly_migrations` history table failed.
    #[error("create migrations table: {0}")]
    CreateTable(#[source] DatabaseError),

    /// Applying a specific migration failed; its transaction was rolled
    /// back and nothing was recorded.
    #[error("V{version} ({filename}): {source}")]
    Apply {
        /// Version of the migration that failed.
        version: i64,
        /// Filename of the migration that failed.
        filename: String,
        /// Underlying database failure.
        #[source]
        source: DatabaseError,
    },

    /// A database operation outside a migration apply failed (e.g.
    /// reading the history table).
    #[error(transparent)]
    Database(#[from] DatabaseError),

    /// Reading migration files from the filesystem failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
