# `firefly-migrations`

> **Tier:** Platform · **Status:** Full · **Java original:** Flyway · **Go module:** `migrations`

## Overview

`firefly-migrations` is the framework's **versioned-SQL migration
runner**. Migration files are named `V{version}__{description}.sql`
(e.g. `V001__init.sql`); each file runs once, in version order, inside
a transaction. The applied versions are recorded in a
`firefly_migrations` table for idempotency.

The runner works against **any** store reachable through the small
synchronous `Database` port — the SQL it issues is parameter-free and
ANSI-compatible (plus one `?`-placeholder insert). Tested against
SQLite via rusqlite, exactly as the Go module tested against
`modernc.org/sqlite`.

## Why a separate module?

Flyway and DbUp are mature in the JVM and .NET worlds. The Rust
ecosystem has several migration crates, each with a slightly different
file-name convention, locking strategy, and recovery story.
`firefly-migrations` provides **one** convention so every Firefly Rust
service handles schema evolution the same way — and the `V###__name.sql`
naming is wire-identical across the Java, Go, .NET, Python, and Rust
ports.

## File layout

```
db/
├── V001__init.sql
├── V002__add_orders_index.sql
└── V003__seed_reference_data.sql
```

The runner only matches files with the `V{version}__{description}.sql`
pattern; everything else (READMEs, .gitkeep) is ignored.

## Public surface

```rust,ignore
pub struct Migration {
    pub version: i64,
    pub description: String,
    pub filename: String,
    pub sql: String,
    pub checksum: String, // hex SHA-256 of the SQL bytes
}

pub trait Source { fn list(&self) -> Result<Vec<Migration>, MigrationError>; }

pub struct DirSource { pub dir: PathBuf }       // filesystem directory
pub struct EmbeddedSource;                      // include_str! pairs (Go's embed.FS)
pub struct SliceSource { pub items: Vec<Migration> } // hand-built (tests)

pub trait Database {                            // sync port over your driver
    fn execute(&mut self, sql: &str, params: &[SqlValue]) -> Result<(), DatabaseError>;
    fn query(&mut self, sql: &str) -> Result<Vec<Vec<SqlValue>>, DatabaseError>;
    fn begin(&mut self) -> Result<(), DatabaseError>;
    fn commit(&mut self) -> Result<(), DatabaseError>;
    fn rollback(&mut self) -> Result<(), DatabaseError>;
}

pub fn run(db: &mut impl Database, src: &impl Source) -> Result<(), MigrationError>;
pub fn inspect(db: &mut impl Database, src: &impl Source) -> Result<Status, MigrationError>;

pub struct Status { pub applied: Vec<Migration>, pub pending: Vec<Migration> }

pub enum MigrationError {
    ChecksumMismatch { version, filename }, // "firefly/migrations: checksum mismatch: V1 (V001__init.sql)"
    CreateTable(DatabaseError),
    Apply { version, filename, source },
    Database(DatabaseError),
    Io(std::io::Error),
}
```

`run` and `inspect` also accept trait objects (`&mut dyn Database`,
`&dyn Source`).

## Schema

```sql
CREATE TABLE IF NOT EXISTS firefly_migrations (
    version     INTEGER     PRIMARY KEY,
    description TEXT        NOT NULL,
    filename    TEXT        NOT NULL,
    checksum    TEXT        NOT NULL,
    applied_at  TIMESTAMP   NOT NULL
);
```

`applied_at` is bound as RFC 3339 UTC text.

## Checksum guard

When a migration is applied, its SHA-256 checksum is stored. If the
file is later edited (something you should **never** do — migrations
are append-only history), a subsequent `run` returns
`MigrationError::ChecksumMismatch` rather than silently skipping.

## Quick start

Adapt your driver to the `Database` port once (rusqlite shown), embed
your migrations with `include_str!` — the analog of Go's `embed.FS` —
and run:

```rust
use firefly_migrations::{inspect, run, Database, DatabaseError, EmbeddedSource, SqlValue};

struct Sqlite(rusqlite::Connection);

fn db_err(e: rusqlite::Error) -> DatabaseError {
    DatabaseError(e.to_string())
}

impl Database for Sqlite {
    fn execute(&mut self, sql: &str, params: &[SqlValue]) -> Result<(), DatabaseError> {
        if params.is_empty() {
            return self.0.execute_batch(sql).map_err(db_err);
        }
        let bound: Vec<&dyn rusqlite::ToSql> = params
            .iter()
            .map(|p| match p {
                SqlValue::Int(i) => i as &dyn rusqlite::ToSql,
                SqlValue::Text(s) => s as &dyn rusqlite::ToSql,
            })
            .collect();
        self.0.execute(sql, bound.as_slice()).map(|_| ()).map_err(db_err)
    }

    fn query(&mut self, sql: &str) -> Result<Vec<Vec<SqlValue>>, DatabaseError> {
        let mut stmt = self.0.prepare(sql).map_err(db_err)?;
        let ncols = stmt.column_count();
        let mut rows = stmt.query([]).map_err(db_err)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let mut rec = Vec::with_capacity(ncols);
            for i in 0..ncols {
                rec.push(match row.get_ref(i).map_err(db_err)? {
                    rusqlite::types::ValueRef::Integer(n) => SqlValue::Int(n),
                    rusqlite::types::ValueRef::Text(t) => {
                        SqlValue::Text(String::from_utf8_lossy(t).into_owned())
                    }
                    other => return Err(DatabaseError(format!("unsupported: {other:?}"))),
                });
            }
            out.push(rec);
        }
        Ok(out)
    }

    fn begin(&mut self) -> Result<(), DatabaseError> {
        self.0.execute_batch("BEGIN").map_err(db_err)
    }
    fn commit(&mut self) -> Result<(), DatabaseError> {
        self.0.execute_batch("COMMIT").map_err(db_err)
    }
    fn rollback(&mut self) -> Result<(), DatabaseError> {
        self.0.execute_batch("ROLLBACK").map_err(db_err)
    }
}

fn main() {
    let mut db = Sqlite(rusqlite::Connection::open_in_memory().unwrap());

    // In a real service: ("V001__init.sql", include_str!("../db/V001__init.sql"))
    let src = EmbeddedSource::new(&[
        ("V001__init.sql", "CREATE TABLE t (id INTEGER)"),
        ("V002__seed.sql", "INSERT INTO t VALUES (1)"),
    ]);

    run(&mut db, &src).unwrap();
    run(&mut db, &src).unwrap(); // idempotent — second run is a no-op

    let st = inspect(&mut db, &src).unwrap();
    println!("applied={} pending={}", st.applied.len(), st.pending.len());
}
```

Hand-built (tests):

```rust,ignore
let src = SliceSource {
    items: vec![Migration {
        version: 1,
        filename: "V001__init.sql".into(),
        sql: "CREATE TABLE t (id INTEGER)".into(),
        ..Default::default() // empty checksum is computed on list()
    }],
};
run(&mut db, &src)?;
```

## Testing

```bash
cargo test -p firefly-migrations
```

Covers fresh apply, idempotent re-run, checksum-mismatch rejection, the
directory and embedded source variants, transactional rollback of a
failed migration, pending/applied inspection, error-string parity with
the Go port, and Send + Sync / trait-object usability.
