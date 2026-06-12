//! The `firefly db` migration command group.
//!
//! Rust port of pyfly's `pyfly.cli.db` (`cli/db.py`). pyfly drives
//! **Alembic** revisions; this port drives the framework's own
//! [`firefly_migrations`] runner instead — the same library the generated
//! projects ship with (`firefly generate migration` writes
//! `V###__name.sql` files into `migrations/`). The subcommand *names* are
//! kept aligned with pyfly so a developer migrating from
//! `pyfly db upgrade` finds `firefly db upgrade`.
//!
//! # Backends
//!
//! The default and fully-testable backend is **SQLite via `rusqlite`**
//! (an in-memory database is used by the test-suite). The database URL is
//! read from `--url`, falling back to the `DATABASE_URL` environment
//! variable, then to `firefly.yaml`'s `firefly.datasource.url`. Accepted
//! SQLite URL forms:
//!
//! - `sqlite::memory:` / `:memory:` — an ephemeral in-memory database;
//! - `sqlite:///abs/path.db` / `sqlite://path.db` / `sqlite:path.db` —
//!   a file-backed database;
//! - a bare filesystem path (`./app.db`).
//!
//! Other backends (Postgres, MySQL) are **not yet wired into the CLI**:
//! the `firefly-migrations` [`Database`] port can adapt any driver, but
//! the convenience CLI ships only the SQLite adapter. A `postgres://`
//! (etc.) URL therefore returns a clear [`CliError::Unsupported`]
//! pointing the operator at the library API. This is a documented
//! divergence from pyfly, whose Alembic env binds an async SQLAlchemy
//! engine for any configured driver.
//!
//! # Command map (pyfly → Rust)
//!
//! | pyfly | Rust | Behaviour |
//! | --- | --- | --- |
//! | `db init` | `db init` | Create `migrations/` + a starter `V001__init.sql`. |
//! | `db migrate -m msg` | `db migrate -m msg` | Write a new empty `V###__msg.sql`. |
//! | `db upgrade [rev]` | `db upgrade` | Apply all pending migrations (`run`). |
//! | `db downgrade rev` | `db downgrade` | **Unsupported** — the runner is forward-only. |
//! | `db current`/`history`/`heads`/`status` | `db status` | Show applied + pending (`inspect`). |
//!
//! `downgrade` is the deliberate divergence: `firefly-migrations` is an
//! append-only, forward-only history (matching the Go port's Flyway-style
//! model), so a rollback to an arbitrary revision has no analog and the
//! command fails loudly rather than silently no-op'ing.

use std::path::{Path, PathBuf};

use firefly_migrations::{
    inspect, run, Database, DatabaseError, DirSource, Source, SqlValue, Status,
};

use crate::error::CliError;

/// Default starter migration written by `db init`.
const INIT_MIGRATION: &str = "-- V001__init.sql\n-- Initial schema migration.\n-- Edit this file, then run `firefly db upgrade`.\n\n-- CREATE TABLE example (\n--     id   INTEGER PRIMARY KEY,\n--     name TEXT NOT NULL\n-- );\n";

/// History-table DDL — kept byte-identical to `firefly_migrations`' own
/// `firefly_migrations` schema so `db status` can read it via `inspect`
/// without applying any migrations (which `run` would otherwise do).
const HISTORY_DDL: &str = "
CREATE TABLE IF NOT EXISTS firefly_migrations (
    version     INTEGER     PRIMARY KEY,
    description TEXT        NOT NULL,
    filename    TEXT        NOT NULL,
    checksum    TEXT        NOT NULL,
    applied_at  TIMESTAMP   NOT NULL
)";

/// Resolve the SQLite database URL from, in priority order: an explicit
/// `--url`, the `DATABASE_URL` environment variable, then
/// `firefly.yaml`'s `firefly.datasource.url`. Falls back to a file-backed
/// `firefly.db` in the working directory when nothing is configured.
pub fn resolve_url(explicit: Option<&str>) -> String {
    if let Some(u) = explicit {
        if !u.is_empty() {
            return u.to_string();
        }
    }
    if let Ok(env_url) = std::env::var("DATABASE_URL") {
        if !env_url.is_empty() {
            return env_url;
        }
    }
    if let Some(url) = datasource_url_from_yaml(Path::new("firefly.yaml")) {
        return url;
    }
    "sqlite://firefly.db".to_string()
}

/// Read `firefly.datasource.url` from a `firefly.yaml` file, if present.
fn datasource_url_from_yaml(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    value
        .get("firefly")?
        .get("datasource")?
        .get("url")?
        .as_str()
        .map(str::to_string)
}

/// A SQLite path the [`rusqlite`] driver can open, parsed from a database
/// URL — or [`None`] when the URL names a non-SQLite backend.
enum SqlitePath {
    /// `sqlite::memory:` / `:memory:`.
    Memory,
    /// A file-backed database at this path.
    File(PathBuf),
}

/// Parse a database URL into a SQLite target.
///
/// Returns `Err(CliError::Unsupported)` for `postgres://`, `mysql://`,
/// etc., naming the unsupported scheme.
fn parse_sqlite_url(url: &str) -> Result<SqlitePath, CliError> {
    let trimmed = url.trim();
    if trimmed == ":memory:" || trimmed == "sqlite::memory:" || trimmed == "sqlite://:memory:" {
        return Ok(SqlitePath::Memory);
    }
    // Strip any sqlite scheme prefix; keep bare paths as-is.
    let path = if let Some(rest) = trimmed.strip_prefix("sqlite://") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("sqlite:") {
        rest
    } else if let Some(scheme_end) = trimmed.find("://") {
        let scheme = &trimmed[..scheme_end];
        return Err(CliError::Unsupported(format!(
            "the '{scheme}' backend is not wired into the `firefly db` CLI \
             (only sqlite is). Adapt the firefly-migrations Database port to your \
             driver and call firefly_migrations::run directly."
        )));
    } else {
        trimmed
    };
    if path.is_empty() {
        return Ok(SqlitePath::Memory);
    }
    Ok(SqlitePath::File(PathBuf::from(path)))
}

/// A [`firefly_migrations::Database`] adapter over a [`rusqlite::Connection`].
///
/// The runner only ever traffics in `INTEGER` and `TEXT` values, so the
/// mapping between [`SqlValue`] and `rusqlite` is exhaustive.
struct SqliteDb(rusqlite::Connection);

impl SqliteDb {
    /// Open the connection named by a parsed SQLite URL.
    fn open(target: SqlitePath) -> Result<Self, CliError> {
        let conn = match target {
            SqlitePath::Memory => rusqlite::Connection::open_in_memory(),
            SqlitePath::File(path) => rusqlite::Connection::open(path),
        }
        .map_err(|e| CliError::Database(format!("could not open database: {e}")))?;
        Ok(Self(conn))
    }
}

fn db_err(e: rusqlite::Error) -> DatabaseError {
    DatabaseError(e.to_string())
}

impl Database for SqliteDb {
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
                    rusqlite::types::ValueRef::Integer(n) => SqlValue::Int(n),
                    rusqlite::types::ValueRef::Text(t) => {
                        SqlValue::Text(String::from_utf8_lossy(t).into_owned())
                    }
                    rusqlite::types::ValueRef::Null => SqlValue::Text(String::new()),
                    other => {
                        return Err(DatabaseError(format!(
                            "unsupported column value: {other:?}"
                        )))
                    }
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

/// Outcome of `firefly db init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// The `migrations/` directory that now exists.
    pub dir: PathBuf,
    /// The starter migration file written (`None` when one already existed).
    pub created: Option<PathBuf>,
}

/// `firefly db init` — create the `migrations/` directory with a starter
/// `V001__init.sql`, the rough analog of `pyfly db init` (which scaffolds
/// the Alembic environment).
///
/// Idempotent: an existing `migrations/` directory is reused; the starter
/// file is written only when no `V###__*.sql` migration exists yet.
///
/// # Errors
/// [`CliError::Io`] when the directory or file cannot be created.
pub fn db_init(dir: &Path) -> Result<InitOutcome, CliError> {
    std::fs::create_dir_all(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let has_migration = std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.file_name().to_string_lossy().as_ref().starts_with('V')
                    && e.file_name().to_string_lossy().ends_with(".sql")
            })
        })
        .unwrap_or(false);
    if has_migration {
        return Ok(InitOutcome {
            dir: dir.to_path_buf(),
            created: None,
        });
    }
    let file = dir.join("V001__init.sql");
    std::fs::write(&file, INIT_MIGRATION).map_err(|source| CliError::Io {
        path: file.clone(),
        source,
    })?;
    Ok(InitOutcome {
        dir: dir.to_path_buf(),
        created: Some(file),
    })
}

/// `firefly db migrate -m <message>` — write a new empty migration file,
/// the analog of `pyfly db migrate` (Alembic `revision`).
///
/// The filename uses the framework's `V###__name.sql` convention with the
/// version auto-incremented from the highest existing migration (matching
/// `firefly generate migration`), so revisions stay totally ordered.
///
/// # Errors
/// [`CliError::Io`] when the directory cannot be read or the file written.
pub fn db_migrate(dir: &Path, message: Option<&str>) -> Result<PathBuf, CliError> {
    std::fs::create_dir_all(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let version = next_version(dir);
    let slug = slugify(message.unwrap_or("migration"));
    let filename = format!("V{version:03}__{slug}.sql");
    let path = dir.join(&filename);
    let body = format!(
        "-- {filename}\n-- {desc}\n\n",
        desc = message.unwrap_or("migration")
    );
    std::fs::write(&path, body).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// `firefly db upgrade` — apply every pending migration, the analog of
/// `pyfly db upgrade head`.
///
/// Returns the number of migrations applied by this call (0 when already
/// up to date — the runner is idempotent).
///
/// # Errors
/// [`CliError::Unsupported`] for a non-SQLite URL, [`CliError::Database`]
/// when the connection cannot be opened or a migration fails to apply.
pub fn db_upgrade(dir: &Path, url: &str) -> Result<usize, CliError> {
    let mut db = SqliteDb::open(parse_sqlite_url(url)?)?;
    let src = DirSource::new(dir);
    // Snapshot the pending count before applying so we can report it; if
    // the history table does not exist yet, `inspect` fails — treat that
    // as "everything is pending".
    let pending_before = inspect(&mut db, &src)
        .map(|s| s.pending.len())
        .unwrap_or_else(|_| src_len(dir));
    run(&mut db, &src).map_err(|e| CliError::Database(e.to_string()))?;
    Ok(pending_before)
}

/// `firefly db status` — report applied + pending migrations, the analog
/// of `pyfly db current` / `history` combined.
///
/// Ensures the history table exists first (running zero migrations is a
/// no-op when nothing is pending), so `status` works on a fresh database.
///
/// # Errors
/// [`CliError::Unsupported`] for a non-SQLite URL, [`CliError::Database`]
/// on a connection or query failure.
pub fn db_status(dir: &Path, url: &str) -> Result<Status, CliError> {
    let mut db = SqliteDb::open(parse_sqlite_url(url)?)?;
    let src = DirSource::new(dir);
    // Create the (idempotent) history table directly so `inspect` can read
    // it on a fresh database — WITHOUT applying any pending migrations
    // (which `run` would do). `status` must be read-only w.r.t. schema.
    db.execute(HISTORY_DDL, &[])
        .map_err(|e| CliError::Database(e.to_string()))?;
    inspect(&mut db, &src).map_err(|e| CliError::Database(e.to_string()))
}

/// `firefly db downgrade` — **unsupported**. The `firefly-migrations`
/// runner is forward-only / append-only (Flyway-style), so there is no
/// rollback to an arbitrary revision. Documented divergence from pyfly's
/// Alembic `downgrade`.
///
/// # Errors
/// Always returns [`CliError::Unsupported`].
pub fn db_downgrade() -> Result<(), CliError> {
    Err(CliError::Unsupported(
        "downgrade is not supported: firefly-migrations is forward-only \
         (append-only history, like Flyway). Write a new corrective \
         migration with `firefly db migrate` instead."
            .to_string(),
    ))
}

/// Count `V###__*.sql` files in `dir` (used when the history table is
/// absent and every listed migration is therefore pending).
fn src_len(dir: &Path) -> usize {
    DirSource::new(dir).list().map(|v| v.len()).unwrap_or(0)
}

/// Compute the next `V###` version by scanning `dir` for existing
/// migrations — shares the numbering rule with the `generate` module.
fn next_version(dir: &Path) -> u32 {
    let highest = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| parse_version(&e.file_name().to_string_lossy()))
        .max()
        .unwrap_or(0);
    highest + 1
}

/// Parse the numeric version from a `V###__name.sql` filename.
fn parse_version(filename: &str) -> Option<u32> {
    let rest = filename.strip_prefix('V')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Lower-case snake-case slug for a migration message (spaces/punctuation
/// collapse to single underscores).
fn slugify(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut prev_underscore = false;
    for ch in message.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    let trimmed = out.trim_end_matches('_');
    if trimmed.is_empty() {
        "migration".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_sqlite_url_variants() {
        assert!(matches!(
            parse_sqlite_url(":memory:"),
            Ok(SqlitePath::Memory)
        ));
        assert!(matches!(
            parse_sqlite_url("sqlite::memory:"),
            Ok(SqlitePath::Memory)
        ));
        match parse_sqlite_url("sqlite:///tmp/app.db") {
            Ok(SqlitePath::File(p)) => assert_eq!(p, PathBuf::from("/tmp/app.db")),
            other => panic!(
                "expected file path, got {other:?}",
                other = match other {
                    Ok(SqlitePath::Memory) => "memory".to_string(),
                    Ok(SqlitePath::File(p)) => format!("file {}", p.display()),
                    Err(e) => format!("err {e}"),
                }
            ),
        }
        match parse_sqlite_url("sqlite:app.db") {
            Ok(SqlitePath::File(p)) => assert_eq!(p, PathBuf::from("app.db")),
            _ => panic!("expected file path"),
        }
        match parse_sqlite_url("./bare.db") {
            Ok(SqlitePath::File(p)) => assert_eq!(p, PathBuf::from("./bare.db")),
            _ => panic!("expected bare path"),
        }
    }

    #[test]
    fn parse_sqlite_url_rejects_other_backends() {
        let err = parse_sqlite_url("postgres://localhost/db");
        assert!(matches!(err, Err(CliError::Unsupported(_))));
        let err = parse_sqlite_url("mysql://localhost/db");
        assert!(matches!(err, Err(CliError::Unsupported(_))));
    }

    #[test]
    fn slugify_normalizes() {
        assert_eq!(slugify("Add Users Table"), "add_users_table");
        assert_eq!(slugify("create-orders!!!"), "create_orders");
        assert_eq!(slugify("  spaced  "), "spaced");
        assert_eq!(slugify("***"), "migration");
    }

    #[test]
    fn parse_version_reads_prefix() {
        assert_eq!(parse_version("V001__init.sql"), Some(1));
        assert_eq!(parse_version("V42__x.sql"), Some(42));
        assert_eq!(parse_version("README.md"), None);
        assert_eq!(parse_version("Vx__bad.sql"), None);
    }

    #[test]
    fn db_init_creates_dir_and_starter() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("migrations");
        let outcome = db_init(&dir).unwrap();
        assert!(dir.is_dir());
        assert_eq!(outcome.created, Some(dir.join("V001__init.sql")));
        assert!(dir.join("V001__init.sql").is_file());
    }

    #[test]
    fn db_init_is_idempotent_when_migration_exists() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("migrations");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("V001__init.sql"), "CREATE TABLE t (id INTEGER)").unwrap();
        let outcome = db_init(&dir).unwrap();
        assert_eq!(outcome.created, None);
        // Existing file is untouched.
        assert_eq!(
            std::fs::read_to_string(dir.join("V001__init.sql")).unwrap(),
            "CREATE TABLE t (id INTEGER)"
        );
    }

    #[test]
    fn db_migrate_increments_version() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("migrations");
        let p1 = db_migrate(&dir, Some("Add Users")).unwrap();
        assert!(p1.ends_with("V001__add_users.sql"));
        let p2 = db_migrate(&dir, Some("Add Orders")).unwrap();
        assert!(p2.ends_with("V002__add_orders.sql"));
        let p3 = db_migrate(&dir, None).unwrap();
        assert!(p3.ends_with("V003__migration.sql"));
    }

    #[test]
    fn upgrade_and_status_against_in_memory_sqlite() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("migrations");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("V001__init.sql"),
            "CREATE TABLE widget (id INTEGER PRIMARY KEY)",
        )
        .unwrap();
        std::fs::write(
            dir.join("V002__seed.sql"),
            "INSERT INTO widget (id) VALUES (1)",
        )
        .unwrap();

        // Status on a fresh in-memory db: everything pending.
        // (A new :memory: db per call, so status reports nothing applied.)
        let st = db_status(&dir, ":memory:").unwrap();
        assert_eq!(st.applied.len(), 0);
        assert_eq!(st.pending.len(), 2);

        // Upgrade against a shared file-backed db so state persists.
        let db_file = tmp.path().join("app.db");
        let url = format!("sqlite://{}", db_file.display());
        let applied = db_upgrade(&dir, &url).unwrap();
        assert_eq!(applied, 2);

        // Second upgrade is idempotent (0 applied).
        let applied = db_upgrade(&dir, &url).unwrap();
        assert_eq!(applied, 0);

        let st = db_status(&dir, &url).unwrap();
        assert_eq!(st.applied.len(), 2);
        assert_eq!(st.pending.len(), 0);
    }

    #[test]
    fn downgrade_is_unsupported() {
        assert!(matches!(db_downgrade(), Err(CliError::Unsupported(_))));
    }

    #[test]
    fn upgrade_rejects_postgres_url() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("migrations");
        std::fs::create_dir_all(&dir).unwrap();
        let err = db_upgrade(&dir, "postgres://localhost/db");
        assert!(matches!(err, Err(CliError::Unsupported(_))));
    }

    #[test]
    fn resolve_url_prefers_explicit() {
        assert_eq!(resolve_url(Some("sqlite://x.db")), "sqlite://x.db");
    }

    #[test]
    fn datasource_url_from_yaml_reads_value() {
        let tmp = TempDir::new().unwrap();
        let yaml = tmp.path().join("firefly.yaml");
        std::fs::write(
            &yaml,
            "firefly:\n  datasource:\n    url: sqlite://from-yaml.db\n",
        )
        .unwrap();
        assert_eq!(
            datasource_url_from_yaml(&yaml).as_deref(),
            Some("sqlite://from-yaml.db")
        );
        assert_eq!(
            datasource_url_from_yaml(tmp.path().join("missing.yaml").as_path()),
            None
        );
    }
}
