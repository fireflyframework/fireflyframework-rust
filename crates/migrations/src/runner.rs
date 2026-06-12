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

//! The migration runner: idempotent apply ([`run`]) and status
//! inspection ([`inspect`]).

use std::collections::HashMap;

use crate::database::{Database, DatabaseError, SqlValue};
use crate::error::MigrationError;
use crate::migration::Migration;
use crate::source::Source;

/// History-table DDL — column-for-column identical to the Go port (and
/// to the schema the Java/.NET/Python ports record).
const DDL_CREATE: &str = "
CREATE TABLE IF NOT EXISTS firefly_migrations (
    version     INTEGER     PRIMARY KEY,
    description TEXT        NOT NULL,
    filename    TEXT        NOT NULL,
    checksum    TEXT        NOT NULL,
    applied_at  TIMESTAMP   NOT NULL
)";

const INSERT_HISTORY: &str = "INSERT INTO firefly_migrations (version, description, filename, checksum, applied_at) VALUES (?, ?, ?, ?, ?)";

const SELECT_APPLIED: &str = "SELECT version, checksum FROM firefly_migrations";

/// Applies every migration not yet recorded in the `firefly_migrations`
/// table. It is idempotent — repeated calls do nothing once everything
/// is up to date.
///
/// Each pending migration runs inside a transaction together with its
/// history-row insert; on failure the transaction is rolled back and
/// [`MigrationError::Apply`] is returned. If an already-applied
/// migration's checksum no longer matches the source,
/// [`MigrationError::ChecksumMismatch`] is returned and nothing further
/// is applied.
pub fn run<D, S>(db: &mut D, src: &S) -> Result<(), MigrationError>
where
    D: Database + ?Sized,
    S: Source + ?Sized,
{
    db.execute(DDL_CREATE, &[])
        .map_err(MigrationError::CreateTable)?;
    let migs = src.list()?;
    let applied = load_applied(db)?;
    for m in &migs {
        if let Some(recorded) = applied.get(&m.version) {
            if *recorded != m.checksum {
                return Err(MigrationError::ChecksumMismatch {
                    version: m.version,
                    filename: m.filename.clone(),
                });
            }
            continue;
        }
        apply_one(db, m).map_err(|source| MigrationError::Apply {
            version: m.version,
            filename: m.filename.clone(),
            source,
        })?;
    }
    Ok(())
}

/// Status returns the list of applied + pending migrations for
/// inspection.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Migrations recorded in the history table.
    pub applied: Vec<Migration>,
    /// Migrations the source lists that have not been applied yet.
    pub pending: Vec<Migration>,
}

/// Returns a [`Status`] snapshot without applying anything.
///
/// Like the Go port, this reads the history table directly, so it fails
/// if [`run`] has never created it on this database.
pub fn inspect<D, S>(db: &mut D, src: &S) -> Result<Status, MigrationError>
where
    D: Database + ?Sized,
    S: Source + ?Sized,
{
    let migs = src.list()?;
    let applied = load_applied(db)?;
    let mut status = Status::default();
    for m in migs {
        if applied.contains_key(&m.version) {
            status.applied.push(m);
        } else {
            status.pending.push(m);
        }
    }
    Ok(status)
}

/// Loads `version -> checksum` for every row in the history table.
fn load_applied<D: Database + ?Sized>(db: &mut D) -> Result<HashMap<i64, String>, MigrationError> {
    let rows = db.query(SELECT_APPLIED)?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        match row.as_slice() {
            [SqlValue::Int(version), SqlValue::Text(checksum)] => {
                out.insert(*version, checksum.clone());
            }
            other => {
                return Err(MigrationError::Database(DatabaseError(format!(
                    "firefly/migrations: unexpected history row shape: {other:?}"
                ))))
            }
        }
    }
    Ok(out)
}

/// Applies one migration transactionally: the migration SQL plus its
/// history-row insert commit together or not at all.
fn apply_one<D: Database + ?Sized>(db: &mut D, m: &Migration) -> Result<(), DatabaseError> {
    db.begin()?;
    if let Err(e) = exec_and_record(db, m) {
        let _ = db.rollback();
        return Err(e);
    }
    db.commit()
}

fn exec_and_record<D: Database + ?Sized>(db: &mut D, m: &Migration) -> Result<(), DatabaseError> {
    db.execute(&m.sql, &[])?;
    let applied_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    db.execute(
        INSERT_HISTORY,
        &[
            SqlValue::Int(m.version),
            SqlValue::Text(m.description.clone()),
            SqlValue::Text(m.filename.clone()),
            SqlValue::Text(m.checksum.clone()),
            SqlValue::Text(applied_at),
        ],
    )
}
