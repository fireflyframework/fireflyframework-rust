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

//! firefly-migrations — the framework's versioned-SQL migration runner.
//!
//! This crate is the Rust port of the Go module `migrations` (Java
//! original: Flyway; .NET counterpart: EF Core migrations / DbUp).
//!
//! Migration files are named `V{version}__{description}.sql` (e.g.
//! `V001__init.sql`); each file runs once, in version order, inside a
//! transaction. The applied versions are recorded in a
//! `firefly_migrations` table for idempotency, together with a SHA-256
//! checksum of the SQL bytes. If a committed migration is later edited
//! (something you should **never** do — migrations are append-only
//! history), a subsequent [`run`] fails with
//! [`MigrationError::ChecksumMismatch`] rather than silently skipping.
//!
//! The runner drives any store through the small synchronous
//! [`Database`] port — the SQL it issues is parameter-free and
//! ANSI-compatible apart from one `?`-placeholder insert — and reads
//! migrations from a [`Source`]:
//!
//! - [`DirSource`] — a filesystem directory (Go's `FSSource` over
//!   `os.DirFS`),
//! - [`EmbeddedSource`] — compile-time embedded files via
//!   [`include_str!`] (the analog of Go's `embed.FS`),
//! - [`SliceSource`] — a hand-built list, useful in tests.
//!
//! # Quick start
//!
//! Adapt your driver to the [`Database`] port (a rusqlite adapter is
//! elided below; see the crate README for the full listing), then point
//! the runner at a source:
//!
//! ```
//! use firefly_migrations::{inspect, run, Database, DatabaseError, Migration, SliceSource, SqlValue};
//! # struct Sqlite(rusqlite::Connection);
//! # fn db_err(e: rusqlite::Error) -> DatabaseError { DatabaseError(e.to_string()) }
//! # impl Database for Sqlite {
//! #     fn execute(&mut self, sql: &str, params: &[SqlValue]) -> Result<(), DatabaseError> {
//! #         if params.is_empty() {
//! #             return self.0.execute_batch(sql).map_err(db_err);
//! #         }
//! #         let bound: Vec<&dyn rusqlite::ToSql> = params
//! #             .iter()
//! #             .map(|p| match p {
//! #                 SqlValue::Int(i) => i as &dyn rusqlite::ToSql,
//! #                 SqlValue::Text(s) => s as &dyn rusqlite::ToSql,
//! #             })
//! #             .collect();
//! #         self.0.execute(sql, bound.as_slice()).map(|_| ()).map_err(db_err)
//! #     }
//! #     fn query(&mut self, sql: &str) -> Result<Vec<Vec<SqlValue>>, DatabaseError> {
//! #         let mut stmt = self.0.prepare(sql).map_err(db_err)?;
//! #         let ncols = stmt.column_count();
//! #         let mut rows = stmt.query([]).map_err(db_err)?;
//! #         let mut out = Vec::new();
//! #         while let Some(row) = rows.next().map_err(db_err)? {
//! #             let mut rec = Vec::with_capacity(ncols);
//! #             for i in 0..ncols {
//! #                 rec.push(match row.get_ref(i).map_err(db_err)? {
//! #                     rusqlite::types::ValueRef::Integer(n) => SqlValue::Int(n),
//! #                     rusqlite::types::ValueRef::Text(t) => {
//! #                         SqlValue::Text(String::from_utf8_lossy(t).into_owned())
//! #                     }
//! #                     other => return Err(DatabaseError(format!("unsupported: {other:?}"))),
//! #                 });
//! #             }
//! #             out.push(rec);
//! #         }
//! #         Ok(out)
//! #     }
//! #     fn begin(&mut self) -> Result<(), DatabaseError> { self.0.execute_batch("BEGIN").map_err(db_err) }
//! #     fn commit(&mut self) -> Result<(), DatabaseError> { self.0.execute_batch("COMMIT").map_err(db_err) }
//! #     fn rollback(&mut self) -> Result<(), DatabaseError> { self.0.execute_batch("ROLLBACK").map_err(db_err) }
//! # }
//! let mut db = Sqlite(rusqlite::Connection::open_in_memory().unwrap());
//! let src = SliceSource {
//!     items: vec![Migration {
//!         version: 1,
//!         description: "init".into(),
//!         filename: "V001__init.sql".into(),
//!         sql: "CREATE TABLE t (id INTEGER)".into(),
//!         ..Default::default() // empty checksum is filled by the source
//!     }],
//! };
//! run(&mut db, &src).unwrap();
//! run(&mut db, &src).unwrap(); // idempotent — the second run is a no-op
//!
//! let st = inspect(&mut db, &src).unwrap();
//! assert_eq!((st.applied.len(), st.pending.len()), (1, 0));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod database;
mod error;
mod migration;
mod runner;
mod source;

pub use database::{Database, DatabaseError, SqlValue};
pub use error::MigrationError;
pub use migration::Migration;
pub use runner::{inspect, run, Status};
pub use source::{DirSource, EmbeddedSource, SliceSource, Source};

/// Framework version stamp.
pub const VERSION: &str = "26.6.4";
