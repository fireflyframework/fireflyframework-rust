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

//! A backend-agnostic row view so **one** [`SqlxRowMapper`] decodes rows
//! from Postgres, MySQL, *and* SQLite — the row-decoding half of the
//! "one codebase, three relational backends" contract.
//!
//! sqlx's [`Row`](sqlx::Row) trait is generic over the database, so a value
//! decoded from a `PgRow` is a different type from one decoded from a
//! `SqliteRow`. Rather than make every mapper generic three ways, this
//! module wraps a borrowed concrete row in the [`AnyRow`] enum and exposes
//! typed, column-name accessors ([`AnyRow::get_str`], [`AnyRow::get_i64`],
//! …) that dispatch internally. A mapper written once against [`AnyRow`]
//! therefore runs unchanged against any of the three backends.

use chrono::{DateTime, Utc};
use firefly_kernel::FireflyError;

/// A borrowed view over a row from any of the three supported backends.
///
/// Construct it from a concrete sqlx row (the repository does this while
/// streaming) and read columns by name through the typed accessors, which
/// dispatch to the underlying backend's `try_get`. This keeps a
/// [`SqlxRowMapper`] free of backend generics.
pub enum AnyRow<'r> {
    /// A borrowed Postgres row.
    #[cfg(feature = "postgres")]
    Postgres(&'r sqlx::postgres::PgRow),
    /// A borrowed MySQL row.
    #[cfg(feature = "mysql")]
    MySql(&'r sqlx::mysql::MySqlRow),
    /// A borrowed SQLite row.
    #[cfg(feature = "sqlite")]
    Sqlite(&'r sqlx::sqlite::SqliteRow),
    /// Lifetime-binding variant kept inhabited only when no backend
    /// feature is enabled; never constructed in practice.
    #[doc(hidden)]
    #[allow(dead_code)]
    _Phantom(std::marker::PhantomData<&'r ()>),
}

/// Maps a decode failure into a 500 [`FireflyError`].
fn decode_err(column: &str, e: sqlx::Error) -> FireflyError {
    FireflyError::internal(format!("firefly/data-sqlx: column '{column}': {e}"))
}

impl<'r> AnyRow<'r> {
    /// Reads a required `String` column by name.
    pub fn get_str(&self, column: &str) -> Result<String, FireflyError> {
        self.try_get::<String>(column)
    }

    /// Reads an optional `String` column by name (`NULL` → `None`).
    pub fn get_opt_str(&self, column: &str) -> Result<Option<String>, FireflyError> {
        self.try_get::<Option<String>>(column)
    }

    /// Reads a required `i64` column by name.
    pub fn get_i64(&self, column: &str) -> Result<i64, FireflyError> {
        self.try_get::<i64>(column)
    }

    /// Reads an optional `i64` column by name.
    pub fn get_opt_i64(&self, column: &str) -> Result<Option<i64>, FireflyError> {
        self.try_get::<Option<i64>>(column)
    }

    /// Reads a required `i32` column by name.
    pub fn get_i32(&self, column: &str) -> Result<i32, FireflyError> {
        self.try_get::<i32>(column)
    }

    /// Reads a required `f64` column by name.
    pub fn get_f64(&self, column: &str) -> Result<f64, FireflyError> {
        self.try_get::<f64>(column)
    }

    /// Reads a required `bool` column by name.
    pub fn get_bool(&self, column: &str) -> Result<bool, FireflyError> {
        self.try_get::<bool>(column)
    }

    /// Reads a required UTC timestamp column by name.
    pub fn get_datetime(&self, column: &str) -> Result<DateTime<Utc>, FireflyError> {
        self.try_get::<DateTime<Utc>>(column)
    }

    /// Reads an optional UTC timestamp column by name (`NULL` → `None`) —
    /// the accessor a soft-delete / audit mapper uses for `deleted_at`,
    /// `created_at`, …
    pub fn get_opt_datetime(&self, column: &str) -> Result<Option<DateTime<Utc>>, FireflyError> {
        self.try_get::<Option<DateTime<Utc>>>(column)
    }

    /// Reads a column by name as any sqlx-decodable type, dispatching to
    /// the concrete backend row. This is the single point every typed
    /// accessor funnels through, and the escape hatch for column types not
    /// covered by a dedicated `get_*`.
    pub fn try_get<T>(&self, column: &str) -> Result<T, FireflyError>
    where
        T: for<'a> TryGetAcross<'a>,
    {
        match self {
            #[cfg(feature = "postgres")]
            AnyRow::Postgres(r) => T::from_pg(r, column),
            #[cfg(feature = "mysql")]
            AnyRow::MySql(r) => T::from_mysql(r, column),
            #[cfg(feature = "sqlite")]
            AnyRow::Sqlite(r) => T::from_sqlite(r, column),
            AnyRow::_Phantom(_) => Err(FireflyError::internal(
                "firefly/data-sqlx: no backend feature enabled",
            )),
        }
    }
}

/// A decode target that can be read out of any of the three backend rows.
///
/// This is the bridge that lets [`AnyRow::try_get`] stay backend-agnostic:
/// every supported column type implements `TryGetAcross` once (via the
/// [`impl_try_get_across!`] macro) and the row dispatches to the matching
/// backend method.
pub trait TryGetAcross<'a>: Sized {
    /// Decodes the value out of a Postgres row.
    #[cfg(feature = "postgres")]
    fn from_pg(row: &sqlx::postgres::PgRow, column: &str) -> Result<Self, FireflyError>;
    /// Decodes the value out of a MySQL row.
    #[cfg(feature = "mysql")]
    fn from_mysql(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<Self, FireflyError>;
    /// Decodes the value out of a SQLite row.
    #[cfg(feature = "sqlite")]
    fn from_sqlite(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Self, FireflyError>;
}

/// Implements [`TryGetAcross`] for a type decodable from every backend.
macro_rules! impl_try_get_across {
    ($($t:ty),* $(,)?) => {
        $(
            impl<'a> TryGetAcross<'a> for $t {
                #[cfg(feature = "postgres")]
                fn from_pg(row: &sqlx::postgres::PgRow, column: &str) -> Result<Self, FireflyError> {
                    use sqlx::Row;
                    row.try_get::<$t, _>(column).map_err(|e| decode_err(column, e))
                }
                #[cfg(feature = "mysql")]
                fn from_mysql(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<Self, FireflyError> {
                    use sqlx::Row;
                    row.try_get::<$t, _>(column).map_err(|e| decode_err(column, e))
                }
                #[cfg(feature = "sqlite")]
                fn from_sqlite(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Self, FireflyError> {
                    use sqlx::Row;
                    row.try_get::<$t, _>(column).map_err(|e| decode_err(column, e))
                }
            }
        )*
    };
}

impl_try_get_across!(
    String,
    Option<String>,
    i64,
    Option<i64>,
    i32,
    Option<i32>,
    f64,
    Option<f64>,
    bool,
    Option<bool>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

/// Decodes a single row from any backend into a domain entity `T` — the
/// backend-agnostic analogue of Spring Data R2DBC's row-mapping function
/// and of firefly-data's tokio-postgres
/// [`RowMapper`](firefly_data::RowMapper).
///
/// Implement it directly, or pass a closure: the trait is blanket
/// implemented for any `Fn(&AnyRow) -> Result<T, FireflyError>`. A mapper is
/// `Send + Sync` because the streaming [`Flux`](firefly_reactive::Flux) it
/// feeds may be driven on any scheduler.
pub trait SqlxRowMapper<T>: Send + Sync {
    /// Decodes one row into a `T`, or fails the stream with a 500
    /// [`FireflyError`].
    fn map_row(&self, row: &AnyRow<'_>) -> Result<T, FireflyError>;
}

impl<T, F> SqlxRowMapper<T> for F
where
    F: Fn(&AnyRow<'_>) -> Result<T, FireflyError> + Send + Sync,
{
    fn map_row(&self, row: &AnyRow<'_>) -> Result<T, FireflyError> {
        self(row)
    }
}
