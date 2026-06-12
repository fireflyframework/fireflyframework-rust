//! Integration tests for the migration runner, driven through a
//! rusqlite adapter — the same role `modernc.org/sqlite` played in the
//! Go module's tests.

use firefly_migrations::{
    inspect, run, Database, DatabaseError, DirSource, EmbeddedSource, Migration, MigrationError,
    SliceSource, Source, SqlValue, Status,
};
use rusqlite::types::ValueRef;
use sha2::{Digest, Sha256};

/// Adapts a [`rusqlite::Connection`] to the crate's [`Database`] port.
struct Sqlite(rusqlite::Connection);

fn db_err(e: rusqlite::Error) -> DatabaseError {
    DatabaseError(e.to_string())
}

impl Database for Sqlite {
    fn execute(&mut self, sql: &str, params: &[SqlValue]) -> Result<(), DatabaseError> {
        if params.is_empty() {
            // Migration bodies may contain several statements.
            return self.0.execute_batch(sql).map_err(db_err);
        }
        let bound: Vec<&dyn rusqlite::ToSql> = params
            .iter()
            .map(|p| match p {
                SqlValue::Int(i) => i as &dyn rusqlite::ToSql,
                SqlValue::Text(s) => s as &dyn rusqlite::ToSql,
            })
            .collect();
        self.0
            .execute(sql, bound.as_slice())
            .map(|_| ())
            .map_err(db_err)
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
                    ValueRef::Integer(n) => SqlValue::Int(n),
                    ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
                    other => return Err(DatabaseError(format!("unsupported column: {other:?}"))),
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

/// Mirrors the Go tests' `openDB`: a fresh file-backed SQLite database
/// in a temp dir (kept alive by returning the `TempDir` guard).
fn open_db() -> (tempfile::TempDir, Sqlite) {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = rusqlite::Connection::open(dir.path().join("test.db")).expect("open sqlite");
    (dir, Sqlite(conn))
}

fn mig(version: i64, description: &str, filename: &str, sql: &str) -> Migration {
    Migration {
        version,
        description: description.into(),
        filename: filename.into(),
        sql: sql.into(),
        ..Default::default()
    }
}

fn count(db: &mut Sqlite, sql: &str) -> i64 {
    match db.query(sql).expect("count query").as_slice() {
        [row] => match row.as_slice() {
            [SqlValue::Int(n)] => *n,
            other => panic!("unexpected row: {other:?}"),
        },
        other => panic!("unexpected rows: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Ports of the Go tests
// ---------------------------------------------------------------------------

/// Port of Go `TestRunFromSliceSource`.
#[test]
fn run_from_slice_source_is_idempotent() {
    let (_guard, mut db) = open_db();
    let src = SliceSource {
        items: vec![
            mig(1, "init", "V001__init.sql", "CREATE TABLE t (id INTEGER)"),
            mig(2, "seed", "V002__seed.sql", "INSERT INTO t VALUES (1)"),
        ],
    };
    run(&mut db, &src).expect("first run");
    // Idempotent — second run is a no-op.
    run(&mut db, &src).expect("second run");
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM t"), 1);
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM firefly_migrations"), 2);
}

/// Port of Go `TestChecksumMismatch`.
#[test]
fn checksum_mismatch_is_rejected() {
    let (_guard, mut db) = open_db();
    let mut src = SliceSource {
        items: vec![mig(1, "", "V001__init.sql", "CREATE TABLE t (id INTEGER)")],
    };
    run(&mut db, &src).expect("first run");
    // Edit the migration in place and re-run — must error.
    src.items[0].sql = "CREATE TABLE t (id TEXT)".into();
    src.items[0].checksum = String::new(); // recompute
    let err = run(&mut db, &src).expect_err("edited migration must be rejected");
    assert!(
        matches!(
            err,
            MigrationError::ChecksumMismatch { version: 1, ref filename } if filename == "V001__init.sql"
        ),
        "want ChecksumMismatch: {err}"
    );
    // Display parity with the Go wrapped error.
    assert_eq!(
        err.to_string(),
        "firefly/migrations: checksum mismatch: V1 (V001__init.sql)"
    );
}

/// Port of Go `TestFSSource` (directory variant).
#[test]
fn dir_source_lists_runs_and_inspects() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("V001__init.sql"),
        "CREATE TABLE t (id INTEGER)",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("V002__seed.sql"),
        "INSERT INTO t VALUES (1)",
    )
    .unwrap();
    std::fs::write(dir.path().join("README.md"), "ignored").unwrap();

    let src = DirSource::new(dir.path());
    let migs = src.list().expect("list");
    assert_eq!(migs.len(), 2, "listed: {migs:?}");
    assert_eq!(migs[0].version, 1);
    assert_eq!(migs[0].description, "init");
    assert_eq!(migs[0].filename, "V001__init.sql");
    assert_eq!(migs[1].version, 2);

    let (_guard, mut db) = open_db();
    run(&mut db, &src).expect("run");
    let st = inspect(&mut db, &src).expect("inspect");
    assert_eq!(
        (st.applied.len(), st.pending.len()),
        (2, 0),
        "status: {st:?}"
    );
}

/// Port of Go `TestFSSource` (embed.FS variant via `EmbeddedSource`).
#[test]
fn embedded_source_lists_runs_and_inspects() {
    let src = EmbeddedSource::new(&[
        ("V002__seed.sql", "INSERT INTO t VALUES (1)"),
        ("V001__init.sql", "CREATE TABLE t (id INTEGER)"),
        ("README.md", "ignored"),
    ]);
    let migs = src.list().expect("list");
    assert_eq!(migs.len(), 2);
    assert_eq!(migs[0].version, 1, "sorted ascending: {migs:?}");

    let (_guard, mut db) = open_db();
    run(&mut db, &src).expect("run");
    let st = inspect(&mut db, &src).expect("inspect");
    assert_eq!((st.applied.len(), st.pending.len()), (2, 0));
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM t"), 1);
}

// ---------------------------------------------------------------------------
// Rust-specific additions
// ---------------------------------------------------------------------------

#[test]
fn inspect_reports_pending_and_run_catches_up() {
    let (_guard, mut db) = open_db();
    let v1 = mig(1, "init", "V001__init.sql", "CREATE TABLE t (id INTEGER)");
    let v2 = mig(2, "seed", "V002__seed.sql", "INSERT INTO t VALUES (1)");

    run(
        &mut db,
        &SliceSource {
            items: vec![v1.clone()],
        },
    )
    .expect("apply v1");

    let both = SliceSource {
        items: vec![v1, v2],
    };
    let st = inspect(&mut db, &both).expect("inspect");
    assert_eq!(st.applied.len(), 1);
    assert_eq!(st.applied[0].version, 1);
    assert_eq!(st.pending.len(), 1);
    assert_eq!(st.pending[0].version, 2);

    // A later run applies only the pending migration.
    run(&mut db, &both).expect("catch up");
    let st = inspect(&mut db, &both).expect("inspect again");
    assert_eq!((st.applied.len(), st.pending.len()), (2, 0));
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM t"), 1);
}

#[test]
fn failed_migration_rolls_back_and_records_nothing() {
    let (_guard, mut db) = open_db();
    let src = SliceSource {
        items: vec![
            mig(1, "init", "V001__init.sql", "CREATE TABLE t (id INTEGER)"),
            mig(2, "bad", "V002__bad.sql", "THIS IS NOT SQL"),
        ],
    };
    let err = run(&mut db, &src).expect_err("bad SQL must fail");
    let msg = err.to_string();
    assert!(
        msg.starts_with("V2 (V002__bad.sql): "),
        "apply error wraps version + filename: {msg}"
    );
    assert!(matches!(err, MigrationError::Apply { version: 2, .. }));

    // V1 committed, V2 left no history row.
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM firefly_migrations"), 1);
    assert_eq!(
        count(
            &mut db,
            "SELECT COUNT(*) FROM firefly_migrations WHERE version = 2"
        ),
        0
    );

    // V2 was never recorded, so fixing it (new checksum) is allowed.
    let fixed = SliceSource {
        items: vec![
            mig(1, "init", "V001__init.sql", "CREATE TABLE t (id INTEGER)"),
            mig(2, "bad", "V002__bad.sql", "INSERT INTO t VALUES (1)"),
        ],
    };
    run(&mut db, &fixed).expect("fixed run");
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM t"), 1);
}

#[test]
fn history_row_records_all_columns() {
    let (_guard, mut db) = open_db();
    let src = SliceSource {
        items: vec![mig(
            3,
            "add orders index",
            "V003__add_orders_index.sql",
            "CREATE TABLE orders (id INTEGER)",
        )],
    };
    run(&mut db, &src).expect("run");

    let rows = db
        .query(
            "SELECT version, description, filename, checksum, applied_at FROM firefly_migrations",
        )
        .expect("select history");
    assert_eq!(rows.len(), 1);
    let expected_checksum = hex::encode(Sha256::digest(b"CREATE TABLE orders (id INTEGER)"));
    assert_eq!(rows[0][0], SqlValue::Int(3));
    assert_eq!(rows[0][1], SqlValue::Text("add orders index".into()));
    assert_eq!(
        rows[0][2],
        SqlValue::Text("V003__add_orders_index.sql".into())
    );
    assert_eq!(rows[0][3], SqlValue::Text(expected_checksum));
    // applied_at is RFC 3339 UTC text.
    match &rows[0][4] {
        SqlValue::Text(ts) => assert!(ts.ends_with('Z') && ts.contains('T'), "applied_at: {ts}"),
        other => panic!("applied_at should be text: {other:?}"),
    }
}

#[test]
fn dir_source_ignores_directories_and_invalid_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("V001__init.sql"),
        "CREATE TABLE t (id INTEGER)",
    )
    .unwrap();
    std::fs::write(dir.path().join("V1_oops.sql"), "not picked up").unwrap();
    std::fs::write(dir.path().join(".gitkeep"), "").unwrap();
    // A directory whose name matches the pattern is still skipped.
    std::fs::create_dir(dir.path().join("V002__dir.sql")).unwrap();

    let migs = DirSource::new(dir.path()).list().expect("list");
    assert_eq!(migs.len(), 1, "listed: {migs:?}");
    assert_eq!(migs[0].version, 1);
}

#[test]
fn dir_source_missing_directory_is_io_error() {
    let err = DirSource::new("/nonexistent/firefly/migrations")
        .list()
        .expect_err("missing dir");
    assert!(matches!(err, MigrationError::Io(_)), "got: {err}");
}

#[test]
fn inspect_before_run_fails_without_history_table() {
    // Faithful to Go: Inspect queries firefly_migrations directly and
    // errors when Run has never created it.
    let (_guard, mut db) = open_db();
    let src = SliceSource {
        items: vec![mig(
            1,
            "init",
            "V001__init.sql",
            "CREATE TABLE t (id INTEGER)",
        )],
    };
    let err = inspect(&mut db, &src).expect_err("no history table yet");
    assert!(matches!(err, MigrationError::Database(_)), "got: {err}");
}

#[test]
fn multi_statement_migration_applies_atomically() {
    let (_guard, mut db) = open_db();
    let src = SliceSource {
        items: vec![mig(
            1,
            "init",
            "V001__init.sql",
            "CREATE TABLE a (id INTEGER);\nCREATE TABLE b (id INTEGER);\nINSERT INTO a VALUES (1);",
        )],
    };
    run(&mut db, &src).expect("run");
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM a"), 1);
    assert_eq!(count(&mut db, "SELECT COUNT(*) FROM b"), 0);
}

#[test]
fn run_and_inspect_accept_trait_objects() {
    let (_guard, mut db) = open_db();
    let src = SliceSource {
        items: vec![mig(
            1,
            "init",
            "V001__init.sql",
            "CREATE TABLE t (id INTEGER)",
        )],
    };
    let dyn_db: &mut dyn Database = &mut db;
    let dyn_src: &dyn Source = &src;
    run(dyn_db, dyn_src).expect("run via dyn");
    let st = inspect(dyn_db, dyn_src).expect("inspect via dyn");
    assert_eq!(st.applied.len(), 1);
}

#[test]
fn error_display_strings_match_go_wrapping() {
    let create = MigrationError::CreateTable(DatabaseError("boom".into()));
    assert_eq!(create.to_string(), "create migrations table: boom");

    let apply = MigrationError::Apply {
        version: 2,
        filename: "V002__seed.sql".into(),
        source: DatabaseError("syntax error".into()),
    };
    assert_eq!(apply.to_string(), "V2 (V002__seed.sql): syntax error");

    let mismatch = MigrationError::ChecksumMismatch {
        version: 7,
        filename: "V007__x.sql".into(),
    };
    assert_eq!(
        mismatch.to_string(),
        "firefly/migrations: checksum mismatch: V7 (V007__x.sql)"
    );
}

#[test]
fn public_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Migration>();
    assert_send_sync::<Status>();
    assert_send_sync::<MigrationError>();
    assert_send_sync::<DatabaseError>();
    assert_send_sync::<SqlValue>();
    assert_send_sync::<SliceSource>();
    assert_send_sync::<DirSource>();
    assert_send_sync::<EmbeddedSource>();
}
