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
