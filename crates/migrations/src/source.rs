//! Migration sources: directory, compile-time embedded, and hand-built.

use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::MigrationError;
use crate::migration::{checksum, Migration};

/// Filename pattern every source enforces: `V{version}__{description}.sql`.
fn name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^V(\d+)__([A-Za-z0-9_-]+)\.sql$").expect("valid regex"))
}

/// Parse a filename into `(version, description)`; `None` when the name
/// does not match the `V{version}__{description}.sql` convention (such
/// files — READMEs, `.gitkeep` — are silently ignored).
fn parse_name(name: &str) -> Option<(i64, String)> {
    let caps = name_re().captures(name)?;
    let version: i64 = caps[1].parse().ok()?;
    Some((version, caps[2].replace('_', " ")))
}

/// Source produces the ordered list of migrations to apply.
/// Implementations satisfy this from a directory, embedded files, or a
/// static slice.
pub trait Source {
    /// List every migration, sorted by ascending version, with checksums
    /// filled in.
    fn list(&self) -> Result<Vec<Migration>, MigrationError>;
}

/// DirSource lists `V###__name.sql` files from a filesystem directory —
/// the Rust analog of Go's `FSSource` over `os.DirFS`.
#[derive(Debug, Clone)]
pub struct DirSource {
    /// Directory containing the migration files.
    pub dir: PathBuf,
}

impl DirSource {
    /// Returns a `DirSource` rooted at `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl Source for DirSource {
    fn list(&self) -> Result<Vec<Migration>, MigrationError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((version, description)) = parse_name(&name) else {
                continue;
            };
            let body = std::fs::read(entry.path())?;
            out.push(Migration {
                version,
                description,
                filename: name,
                checksum: checksum(&body),
                sql: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        out.sort_by_key(|m| m.version);
        Ok(out)
    }
}

/// EmbeddedSource lists migrations from compile-time embedded files —
/// the Rust analog of Go's `FSSource` over an `embed.FS`. Pair it with
/// [`include_str!`]:
///
/// ```rust,ignore
/// let src = EmbeddedSource::new(&[
///     ("V001__init.sql", include_str!("../db/V001__init.sql")),
///     ("V002__seed.sql", include_str!("../db/V002__seed.sql")),
/// ]);
/// ```
///
/// Names that do not match the `V{version}__{description}.sql` pattern
/// are ignored at list time, exactly like [`DirSource`].
#[derive(Debug, Clone, Default)]
pub struct EmbeddedSource {
    files: Vec<(String, String)>,
}

impl EmbeddedSource {
    /// Returns an `EmbeddedSource` over `(filename, sql)` pairs.
    pub fn new(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(name, sql)| ((*name).to_string(), (*sql).to_string()))
                .collect(),
        }
    }
}

impl Source for EmbeddedSource {
    fn list(&self) -> Result<Vec<Migration>, MigrationError> {
        let mut out = Vec::new();
        for (name, sql) in &self.files {
            let Some((version, description)) = parse_name(name) else {
                continue;
            };
            out.push(Migration {
                version,
                description,
                filename: name.clone(),
                sql: sql.clone(),
                checksum: checksum(sql.as_bytes()),
            });
        }
        out.sort_by_key(|m| m.version);
        Ok(out)
    }
}

/// SliceSource is a hand-built list of migrations — useful in tests.
///
/// Entries with an empty [`Migration::checksum`] have it computed from
/// the SQL bytes on [`list`](Source::list); pre-set checksums are kept
/// as-is.
#[derive(Debug, Clone, Default)]
pub struct SliceSource {
    /// The migrations to serve, in any order.
    pub items: Vec<Migration>,
}

impl Source for SliceSource {
    fn list(&self) -> Result<Vec<Migration>, MigrationError> {
        let mut out = self.items.clone();
        for m in &mut out {
            if m.checksum.is_empty() {
                m.checksum = checksum(m.sql.as_bytes());
            }
        }
        out.sort_by_key(|m| m.version);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_name_matches_convention() {
        assert_eq!(parse_name("V001__init.sql"), Some((1, "init".to_string())));
        assert_eq!(
            parse_name("V002__add_orders_index.sql"),
            Some((2, "add orders index".to_string()))
        );
        assert_eq!(
            parse_name("V20260612__a-b_c.sql"),
            Some((20260612, "a-b c".to_string()))
        );
    }

    #[test]
    fn parse_name_rejects_non_migrations() {
        for name in [
            "README.md",
            ".gitkeep",
            "init.sql",
            "V1_oops.sql",         // single underscore
            "Vx__init.sql",        // non-numeric version
            "V001__has space.sql", // space in description
            "V001__init.SQL",      // wrong extension case
            "xV001__init.sql",     // prefix junk
            "V001__init.sql.bak",  // suffix junk
        ] {
            assert_eq!(parse_name(name), None, "{name} should not match");
        }
    }

    #[test]
    fn slice_source_fills_missing_checksums_and_sorts() {
        let src = SliceSource {
            items: vec![
                Migration {
                    version: 2,
                    filename: "V002__seed.sql".into(),
                    sql: "INSERT INTO t VALUES (1)".into(),
                    checksum: "precomputed".into(),
                    ..Default::default()
                },
                Migration {
                    version: 1,
                    filename: "V001__init.sql".into(),
                    sql: "CREATE TABLE t (id INTEGER)".into(),
                    ..Default::default()
                },
            ],
        };
        let migs = src.list().unwrap();
        assert_eq!(migs.len(), 2);
        // Sorted ascending by version.
        assert_eq!(migs[0].version, 1);
        assert_eq!(migs[1].version, 2);
        // Empty checksum recomputed from the SQL bytes…
        assert_eq!(migs[0].checksum, checksum(b"CREATE TABLE t (id INTEGER)"));
        // …pre-set checksum preserved.
        assert_eq!(migs[1].checksum, "precomputed");
    }

    #[test]
    fn embedded_source_filters_and_sorts() {
        let src = EmbeddedSource::new(&[
            ("V002__seed.sql", "INSERT INTO t VALUES (1)"),
            ("README.md", "ignored"),
            ("V001__init.sql", "CREATE TABLE t (id INTEGER)"),
        ]);
        let migs = src.list().unwrap();
        assert_eq!(migs.len(), 2);
        assert_eq!(migs[0].version, 1);
        assert_eq!(migs[0].description, "init");
        assert_eq!(migs[0].checksum, checksum(b"CREATE TABLE t (id INTEGER)"));
        assert_eq!(migs[1].version, 2);
        assert_eq!(migs[1].description, "seed");
    }
}
